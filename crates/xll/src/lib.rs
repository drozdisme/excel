//! BatchCalc Excel add-in (XLL). Adds a "BatchCalc" menu (Add-ins tab) with the commands of
//! every installed contract. A command snapshots the active workbook (SaveCopyAs, so unsaved
//! edits count), computes all cases on background threads while Excel stays responsive, then
//! writes the results into the workbook's own output sheets.
#![allow(clippy::missing_safety_doc)]
mod xlcall;

use batchcalc::{Contract, RunOptions};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use xlcall::*;
use xlcalc::eval::SheetPatch;
use xlcalc::Scalar;
use xlmodel::{Cell, CellValue, Rect};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const MAX_CMDS: usize = 24;
const VERSION: &str = env!("CARGO_PKG_VERSION");

struct MenuItem {
    contract: Contract,
    command: String,
    label: String,
}

/// What the worker hands back to Excel's main thread.
struct WriteSet {
    sheets: Vec<(String, SheetPatch, Vec<Rect>)>,
    state: Vec<(String, Cell, Scalar)>,
    summary: String,
    cells: usize,
}

struct Running {
    label: String,
    book: String,
    started: Instant,
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<String>>,
    handle: Option<std::thread::JoinHandle<Result<WriteSet, String>>>,
}

static MENU: Mutex<Vec<MenuItem>> = Mutex::new(Vec::new());
static JOB: Mutex<Option<Running>> = Mutex::new(None);
static XLL_PATH: Mutex<String> = Mutex::new(String::new());

// ------------------------------------------------------------------ helpers

fn log(msg: &str) {
    if let Some(dir) = std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("BatchCalc")) {
        let _ = std::fs::create_dir_all(&dir);
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("batchcalc.log")) {
            let _ = writeln!(f, "[{:?}] {msg}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
        }
    }
}

fn call_s(func: i32, texts: &[&str]) -> Result<XlResult, i32> {
    let mut strs: Vec<XlStr> = texts.iter().map(|t| XlStr::new(t)).collect();
    let mut args: Vec<XLOPER12> = strs.iter_mut().map(|s| s.oper()).collect();
    excel(func, &mut args)
}

fn status(text: &str) {
    let mut s = XlStr::new(text);
    let _ = excel(xlcMessage, &mut [boolean(true), s.oper()]);
}
fn clear_status() {
    let _ = excel(xlcMessage, &mut [boolean(false)]);
}
#[link(name = "user32")]
unsafe extern "system" {
    fn IsWindowVisible(hwnd: isize) -> i32;
}

/// Modal dialogs only when Excel is visible (automation/hidden instances must not block).
fn alert(text: &str, kind: f64) {
    log(&format!("alert: {text}"));
    let visible = excel(xlGetHwnd, &mut []).ok().and_then(|r| r.num()).is_some_and(|h| unsafe { IsWindowVisible(h as isize) } != 0);
    if !visible {
        status(&format!("BatchCalc: {}", text.replace('\n', " ")));
        return;
    }
    let mut s = XlStr::new(text);
    let _ = excel(xlcAlert, &mut [s.oper(), num(kind)]);
}

fn guard(f: impl FnOnce() -> i32) -> i32 {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(e) => {
            let msg = e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
            log(&format!("panic: {msg}"));
            alert(&format!("BatchCalc internal error: {msg}"), 3.0);
            0
        }
    }
}

fn contract_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    let xll = XLL_PATH.lock().unwrap().clone();
    if let Some(d) = Path::new(&xll).parent() {
        v.push(d.join("contracts"));
    }
    if let Some(a) = std::env::var_os("APPDATA") {
        v.push(PathBuf::from(a).join("BatchCalc").join("contracts"));
    }
    v
}

fn load_contracts() -> Vec<Contract> {
    let mut out: Vec<Contract> = Vec::new();
    for d in contract_dirs() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
        files.sort();
        for p in files {
            match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|t| Contract::from_json(&t)) {
                Ok(c) if !out.iter().any(|o| o.name == c.name) => out.push(c),
                Ok(_) => {}
                Err(e) => log(&format!("{}: {e}", p.display())),
            }
        }
    }
    out
}

// ------------------------------------------------------------------ Excel entry points

#[unsafe(no_mangle)]
pub extern "system" fn xlAutoOpen() -> i32 {
    guard(|| {
        if let Ok(r) = excel(xlGetName, &mut []) {
            *XLL_PATH.lock().unwrap() = r.text().unwrap_or_default();
        }
        let dll = XLL_PATH.lock().unwrap().clone();
        let register = |proc_name: &str| {
            let _ = call_s_reg(&dll, proc_name);
        };
        for i in 0..MAX_CMDS {
            register(&format!("BC_Cmd{i}"));
        }
        for p in ["BC_Poll", "BC_Cancel", "BC_About"] {
            register(p);
        }
        let contracts = load_contracts();
        let mut menu = MENU.lock().unwrap();
        menu.clear();
        for c in contracts {
            for cmd in &c.commands {
                if menu.len() < MAX_CMDS {
                    menu.push(MenuItem { contract: c.clone(), command: cmd.name.clone(), label: cmd.label.clone() });
                }
            }
        }
        build_menu(&menu);
        log(&format!("loaded {dll} v{VERSION}, {} commands", menu.len()));
        1
    })
}

fn call_s_reg(dll: &str, proc_name: &str) -> Result<XlResult, i32> {
    // REGISTER(module, procedure, type, function_text, argument_text, macro_type=2 command, category)
    let mut s: Vec<XlStr> = [dll, proc_name, "J", proc_name, "", "", "BatchCalc"].iter().map(|t| XlStr::new(t)).collect();
    let mut args: Vec<XLOPER12> = s.iter_mut().map(|x| x.oper()).collect();
    args[5] = num(2.0);
    excel(xlfRegister, &mut args)
}

fn build_menu(items: &[MenuItem]) {
    let _ = excel(xlfDeleteMenu, &mut [num(10.0), XlStr::new("BatchCalc").oper()]);
    let mut rows: Vec<[String; 5]> = vec![["BatchCalc".into(), String::new(), String::new(), String::new(), String::new()]];
    let mut last = String::new();
    for (i, it) in items.iter().enumerate() {
        if it.contract.name != last && !last.is_empty() {
            rows.push(["-".into(), String::new(), String::new(), String::new(), String::new()]);
        }
        last = it.contract.name.clone();
        rows.push([it.label.clone(), format!("BC_Cmd{i}"), String::new(), format!("{} - {}", it.contract.name, it.label), String::new()]);
    }
    if items.is_empty() {
        rows.push(["(no contracts installed)".into(), "BC_About".into(), String::new(), String::new(), String::new()]);
    }
    rows.push(["-".into(), String::new(), String::new(), String::new(), String::new()]);
    rows.push(["Cancel running calculation".into(), "BC_Cancel".into(), String::new(), String::new(), String::new()]);
    rows.push([format!("About BatchCalc {VERSION}"), "BC_About".into(), String::new(), String::new(), String::new()]);
    let mut strs: Vec<XlStr> = rows.iter().flatten().map(|t| XlStr::new(t)).collect();
    let mut cells: Vec<XLOPER12> = strs.iter_mut().map(|s| if unsafe { *s.oper().val.str_ } == 0 { nil() } else { s.oper() }).collect();
    let multi = XLOPER12 { val: Val { array: Arr { lparray: cells.as_mut_ptr(), rows: rows.len() as i32, columns: 5 } }, xltype: xltypeMulti };
    if let Err(rc) = excel(xlfAddMenu, &mut [num(10.0), multi]) {
        log(&format!("ADD.MENU failed: {rc}"));
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn xlAutoClose() -> i32 {
    guard(|| {
        if let Some(r) = JOB.lock().unwrap().as_ref() {
            r.cancel.store(true, Ordering::Relaxed);
        }
        let _ = excel(xlfDeleteMenu, &mut [num(10.0), XlStr::new("BatchCalc").oper()]);
        1
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn xlAutoAdd() -> i32 {
    1
}
#[unsafe(no_mangle)]
pub extern "system" fn xlAutoRemove() -> i32 {
    1
}

macro_rules! commands {
    ($($name:ident = $i:expr),*) => {$(
        #[unsafe(no_mangle)]
        pub extern "system" fn $name() -> i32 { guard(|| start($i)) }
    )*};
}
commands!(BC_Cmd0 = 0, BC_Cmd1 = 1, BC_Cmd2 = 2, BC_Cmd3 = 3, BC_Cmd4 = 4, BC_Cmd5 = 5, BC_Cmd6 = 6, BC_Cmd7 = 7, BC_Cmd8 = 8,
    BC_Cmd9 = 9, BC_Cmd10 = 10, BC_Cmd11 = 11, BC_Cmd12 = 12, BC_Cmd13 = 13, BC_Cmd14 = 14, BC_Cmd15 = 15, BC_Cmd16 = 16,
    BC_Cmd17 = 17, BC_Cmd18 = 18, BC_Cmd19 = 19, BC_Cmd20 = 20, BC_Cmd21 = 21, BC_Cmd22 = 22, BC_Cmd23 = 23);

#[unsafe(no_mangle)]
pub extern "system" fn BC_About() -> i32 {
    guard(|| {
        let menu = MENU.lock().unwrap();
        let dirs: Vec<String> = contract_dirs().iter().map(|d| d.display().to_string()).collect();
        alert(
            &format!(
                "BatchCalc {VERSION}\nNative batch calculation for Excel workbooks.\n\n{} command(s) installed.\nContracts: {}\nLog: %LOCALAPPDATA%\\BatchCalc\\batchcalc.log",
                menu.len(),
                dirs.join("; ")
            ),
            2.0,
        );
        1
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn BC_Cancel() -> i32 {
    guard(|| {
        match JOB.lock().unwrap().as_ref() {
            Some(r) => {
                r.cancel.store(true, Ordering::Relaxed);
                status("BatchCalc: cancelling...");
            }
            None => alert("No BatchCalc calculation is running.", 2.0),
        }
        1
    })
}

fn active_book() -> Option<String> {
    // GET.DOCUMENT(76) -> "[Book.xlsm]Sheet"
    let r = excel(xlfGetDocument, &mut [num(76.0)]).ok()?;
    let t = r.text()?;
    let s = t.strip_prefix('[')?;
    Some(s[..s.find(']')?].to_string())
}

fn book_sheets(book: &str) -> Vec<String> {
    // GET.WORKBOOK(1, book) -> array of "[Book]Sheet"
    let Ok(r) = call_s_workbook(book) else { return vec![] };
    let mut out = Vec::new();
    if r.0.xltype & 0xFFF == xltypeMulti {
        unsafe {
            let a = r.0.val.array;
            for k in 0..(a.rows * a.columns) as usize {
                let x = &*a.lparray.add(k);
                if x.xltype & 0xFFF == xltypeStr {
                    let p = x.val.str_;
                    let t = String::from_utf16_lossy(std::slice::from_raw_parts(p.add(1), *p as usize));
                    out.push(t.rsplit(']').next().unwrap_or(&t).to_string());
                }
            }
        }
    }
    out
}

fn call_s_workbook(book: &str) -> Result<XlResult, i32> {
    let mut s = XlStr::new(book);
    excel(xlfGetWorkbook, &mut [num(1.0), s.oper()])
}

fn schedule_poll() {
    if let Ok(now) = excel(xlfNow, &mut [])
        && let Some(t) = now.num() {
            let mut s = XlStr::new("BC_Poll");
            let _ = excel(xlcOnTime, &mut [num(t + 0.5 / 86400.0), s.oper()]);
        }
}

fn start(i: usize) -> i32 {
    if JOB.lock().unwrap().is_some() {
        alert("A BatchCalc calculation is already running. Wait for it or use BatchCalc > Cancel.", 2.0);
        return 1;
    }
    let (contract, command, label) = {
        let menu = MENU.lock().unwrap();
        let Some(it) = menu.get(i) else { return 1 };
        (it.contract.clone(), it.command.clone(), it.label.clone())
    };
    let Some(book) = active_book() else {
        alert("Open and activate the workbook to calculate first.", 2.0);
        return 1;
    };
    let sheets = book_sheets(&book);
    let missing: Vec<&String> = contract.required_sheets.iter().filter(|r| !sheets.iter().any(|s| s.eq_ignore_ascii_case(r))).collect();
    if !missing.is_empty() {
        alert(&format!("'{book}' is not a '{}' workbook (missing sheets: {}).", contract.name, missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")), 3.0);
        return 1;
    }
    let stages = match batchcalc::command_stages(&contract, &command) {
        Ok(s) => s,
        Err(e) => {
            alert(&e, 3.0);
            return 1;
        }
    };
    // Snapshot including unsaved edits.
    status(&format!("BatchCalc: {label} - taking a snapshot of {book}..."));
    let ext = Path::new(&book).extension().and_then(|e| e.to_str()).unwrap_or("xlsx").to_string();
    let dir = std::env::temp_dir().join("BatchCalc");
    let _ = std::fs::create_dir_all(&dir);
    let snap = dir.join(format!("snapshot_{}_{}.{ext}", std::process::id(), Instant::now().elapsed().as_nanos() ^ (book.len() as u128)));
    let snap_s = snap.display().to_string();
    if let Err(rc) = call_s(xlcSaveCopyAs, &[&snap_s]) {
        clear_status();
        alert(&format!("Could not snapshot the workbook (SaveCopyAs failed, code {rc})."), 3.0);
        return 1;
    }
    if !snap.exists() {
        clear_status();
        alert("Could not snapshot the workbook (no file written).", 3.0);
        return 1;
    }
    if ext.eq_ignore_ascii_case("xlsb") || ext.eq_ignore_ascii_case("xls") {
        let _ = std::fs::remove_file(&snap);
        clear_status();
        alert("This version reads .xlsx/.xlsm workbooks. Save a copy as .xlsm to use BatchCalc.", 3.0);
        return 1;
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(Mutex::new(String::from("reading workbook")));
    let (c2, p2, l2) = (cancel.clone(), progress.clone(), label.clone());
    log(&format!("start '{label}' on {book}"));
    let handle = std::thread::Builder::new()
        .name("batchcalc".into())
        .stack_size(256 << 20)
        .spawn(move || {
            let r = compute(&snap, &contract, &stages, c2, p2, &l2);
            let _ = std::fs::remove_file(&snap);
            r
        })
        .expect("spawn");
    *JOB.lock().unwrap() = Some(Running { label, book, started: Instant::now(), cancel, progress, handle: Some(handle) });
    schedule_poll();
    1
}

fn compute(snap: &Path, contract: &Contract, stages: &[String], cancel: Arc<AtomicBool>, progress: Arc<Mutex<String>>, label: &str) -> Result<WriteSet, String> {
    let t0 = Instant::now();
    let names = batchcalc::sheet_names(snap)?;
    let skip = batchcalc::fully_cleared_sheets(contract, stages);
    let load: Vec<String> = names.iter().filter(|n| !skip.iter().any(|s| s.eq_ignore_ascii_case(n))).cloned().collect();
    let wb = xlmodel::read_workbook(snap, &xlmodel::ReadOptions { sheets: Some(load) }).map_err(|e| e.to_string())?;
    // Extents of fully-cleared sheets (to clear exactly their used area in Excel).
    let full = xlmodel::read_workbook(snap, &xlmodel::ReadOptions { sheets: Some(skip.clone()) }).map_err(|e| e.to_string())?;
    let model = xlcalc::Model::new(wb)?;
    *progress.lock().unwrap() = "calculating".into();
    let p3 = progress.clone();
    let opts = RunOptions {
        cancel,
        progress: Some(Arc::new(move |p: &batchcalc::Progress| {
            *p3.lock().unwrap() = format!("stage {}/{} {} - {}/{} jobs", p.stage_index + 1, p.stage_count, p.stage, p.done, p.total);
        })),
        ..Default::default()
    };
    let result = batchcalc::run(&model, contract, stages, &opts).map_err(|e| e.to_string())?;
    let mut sheets = Vec::new();
    let mut cells = 0;
    for (s, p) in result.patches {
        let name = model.wb.sheets[s].name.clone();
        let mut clears = p.cleared.clone();
        if p.cleared_all {
            let fs = full.sheet(&name);
            let (mr, mc) = fs.map(|x| (x.max_row(), x.max_col())).unwrap_or((0, 0));
            let (pr, pc) = p.extent();
            let (r, c) = (mr.max(pr).max(1), mc.max(pc).max(1));
            clears.push(Rect { r1: 1, c1: 1, r2: r, c2: c });
        }
        cells += p.rows.iter().map(|r| r.iter().filter(|v| v.is_some()).count()).sum::<usize>();
        sheets.push((name, p, clears));
    }
    let state = result.final_state.iter().map(|(s, c, v)| (model.wb.sheets[*s].name.clone(), *c, v.clone())).collect();
    let summary = result.reports.iter().map(|r| format!("{} {:.1}s", r.name, r.seconds)).collect::<Vec<_>>().join(", ");
    log(&format!("computed '{label}' in {:.2}s: {summary}", t0.elapsed().as_secs_f64()));
    Ok(WriteSet { sheets, state, summary, cells })
}

#[unsafe(no_mangle)]
pub extern "system" fn BC_Poll() -> i32 {
    guard(|| {
        let mut guard = JOB.lock().unwrap();
        let Some(job) = guard.as_mut() else { return 1 };
        let finished = job.handle.as_ref().is_some_and(|h| h.is_finished());
        if !finished {
            let p = job.progress.lock().unwrap().clone();
            status(&format!("BatchCalc: {} - {} ({:.0} s)   [BatchCalc > Cancel to stop]", job.label, p, job.started.elapsed().as_secs_f64()));
            drop(guard);
            schedule_poll();
            return 1;
        }
        let job = guard.take().unwrap();
        drop(guard);
        let res = job.handle.unwrap().join().unwrap_or_else(|_| Err("calculation thread crashed".into()));
        match res {
            Ok(ws) => {
                let t = Instant::now();
                match write_back(&job.book, &ws) {
                    Ok(()) => {
                        let msg = format!(
                            "BatchCalc: {} completed in {:.1} s ({:.1} M cells written; {}; write {:.1} s)",
                            job.label,
                            job.started.elapsed().as_secs_f64(),
                            ws.cells as f64 / 1e6,
                            ws.summary,
                            t.elapsed().as_secs_f64()
                        );
                        log(&msg);
                        status(&msg);
                    }
                    Err(e) => {
                        clear_status();
                        log(&format!("write failed: {e}"));
                        alert(&format!("BatchCalc could not write results: {e}"), 3.0);
                    }
                }
            }
            Err(e) => {
                clear_status();
                log(&format!("failed: {e}"));
                if e == "cancelled" {
                    status("BatchCalc: cancelled, the workbook was not changed.");
                } else {
                    alert(&format!("BatchCalc: {} failed. The workbook was not changed.\n\n{e}", job.label), 3.0);
                }
            }
        }
        1
    })
}

// ------------------------------------------------------------------ writing results

fn sheet_id(book: &str, sheet: &str) -> Result<usize, String> {
    let r = call_s(xlSheetId, &[&format!("[{book}]{sheet}")]).map_err(|rc| format!("sheet '{sheet}' not found ({rc})"))?;
    if r.0.xltype & 0xFFF != xltypeRef {
        return Err(format!("sheet '{sheet}' not found"));
    }
    Ok(unsafe { r.0.val.mref.id_sheet })
}

fn set_range(id: usize, rect: Rect, values: Option<&[&CellValue]>) -> Result<(), String> {
    let mut rf = XlRef::new(id, rect.r1, rect.c1, rect.r2, rect.c2);
    let rc = match values {
        None => excel(xlSet, &mut [rf.oper()]),
        Some(vals) => {
            let mut strs: Vec<Vec<u16>> = Vec::new();
            for v in vals {
                if let CellValue::Text(t) = v {
                    let mut s: Vec<u16> = t.encode_utf16().take(32767).collect();
                    s.insert(0, s.len() as u16);
                    strs.push(s);
                }
            }
            let mut k = 0;
            let mut cells: Vec<XLOPER12> = vals
                .iter()
                .map(|v| match v {
                    CellValue::Empty => nil(),
                    CellValue::Number(n) => num(*n),
                    CellValue::Bool(b) => boolean(*b),
                    CellValue::Error(e) => err(match e {
                        xlmodel::XlError::Null => xlerrNull,
                        xlmodel::XlError::Div0 => xlerrDiv0,
                        xlmodel::XlError::Ref => xlerrRef,
                        xlmodel::XlError::Name => xlerrName,
                        xlmodel::XlError::Num => xlerrNum,
                        xlmodel::XlError::NA => xlerrNA,
                        xlmodel::XlError::GettingData => xlerrGettingData,
                        _ => xlerrValue,
                    }),
                    CellValue::Text(_) => {
                        let p = strs[k].as_mut_ptr();
                        k += 1;
                        XLOPER12 { val: Val { str_: p }, xltype: xltypeStr }
                    }
                })
                .collect();
            let multi = XLOPER12 { val: Val { array: Arr { lparray: cells.as_mut_ptr(), rows: rect.rows() as i32, columns: rect.cols() as i32 } }, xltype: xltypeMulti };
            excel(xlSet, &mut [rf.oper(), multi])
        }
    };
    rc.map(|_| ()).map_err(|rc| format!("xlSet failed ({rc}) at R{}C{}", rect.r1, rect.c1))
}

/// Enter a value like typing it (FORMULA command): works on merged cells, which xlSet refuses.
/// Text gets a leading apostrophe so it stays text (as the VBA macros do for case IDs).
fn enter_value(id: usize, c: Cell, v: &CellValue) -> Result<(), String> {
    let mut rf = XlRef::new(id, c.row, c.col, c.row, c.col);
    let mut s;
    let val = match v {
        CellValue::Empty => {
            s = XlStr::new("");
            s.oper()
        }
        CellValue::Number(n) => num(*n),
        CellValue::Bool(b) => boolean(*b),
        CellValue::Text(t) => {
            s = XlStr::new(&format!("'{t}"));
            s.oper()
        }
        CellValue::Error(e) => {
            s = XlStr::new(e.as_str());
            s.oper()
        }
    };
    excel(xlcFormula, &mut [val, rf.oper()]).map(|_| ()).map_err(|rc| format!("cannot write R{}C{} ({rc})", c.row, c.col))
}

/// Maximal rectangles covering exactly the written cells (row runs merged vertically).
fn rectangles(p: &SheetPatch) -> Vec<Rect> {
    let mut done: Vec<Rect> = Vec::new();
    let mut open: Vec<Rect> = Vec::new();
    for (ri, line) in p.rows.iter().enumerate() {
        let row = ri as u32 + 1;
        let mut runs = Vec::new();
        let mut c = 0;
        while c < line.len() {
            if line[c].is_some() {
                let s = c;
                while c < line.len() && line[c].is_some() {
                    c += 1;
                }
                runs.push((s as u32 + 1, c as u32));
            } else {
                c += 1;
            }
        }
        let mut next_open = Vec::new();
        for (c1, c2) in runs {
            if let Some(pos) = open.iter().position(|r| r.c1 == c1 && r.c2 == c2 && r.r2 + 1 == row) {
                let mut r = open.remove(pos);
                r.r2 = row;
                next_open.push(r);
            } else {
                next_open.push(Rect { r1: row, c1, r2: row, c2 });
            }
        }
        done.append(&mut open);
        open = next_open;
    }
    done.append(&mut open);
    done
}

fn write_back(book: &str, ws: &WriteSet) -> Result<(), String> {
    // Manual calculation and no screen updates while writing; restored afterwards.
    let mode = excel(xlfGetDocument, &mut [num(14.0)]).ok().and_then(|r| r.num()).unwrap_or(1.0);
    let _ = excel(xlcOptionsCalculation, &mut [num(3.0)]);
    let _ = excel(xlcEcho, &mut [boolean(false)]);
    let result = (|| -> Result<(), String> {
        let total = ws.sheets.len();
        for (k, (name, patch, clears)) in ws.sheets.iter().enumerate() {
            status(&format!("BatchCalc: writing results {}/{} ({name})...", k + 1, total));
            let id = sheet_id(book, name)?;
            for r in clears {
                set_range(id, *r, None)?;
            }
            for rect in rectangles(patch) {
                // Chunk large blocks by rows (~256k cells each).
                let rows_per = ((256 * 1024) / rect.cols().max(1)).max(1);
                let mut r1 = rect.r1;
                while r1 <= rect.r2 {
                    let r2 = (r1 + rows_per - 1).min(rect.r2);
                    let mut vals: Vec<&CellValue> = Vec::with_capacity(((r2 - r1 + 1) * rect.cols()) as usize);
                    for row in r1..=r2 {
                        for col in rect.c1..=rect.c2 {
                            vals.push(patch.written(Cell { row, col }).expect("rect covers written cells"));
                        }
                    }
                    let block = Rect { r1, r2, ..rect };
                    if set_range(id, block, Some(&vals)).is_err() {
                        // Blocks crossing merged cells are refused by xlSet; write cell by cell
                        // (like VBA's Cells(r, c).Value2 = ...), merged followers are skipped.
                        let mut k = 0;
                        for row in block.r1..=block.r2 {
                            for col in block.c1..=block.c2 {
                                let c = Rect::single(Cell { row, col });
                                if set_range(id, c, Some(&[vals[k]])).is_err() {
                                    enter_value(id, Cell { row, col }, vals[k])?;
                                }
                                k += 1;
                            }
                        }
                    }
                    r1 = r2 + 1;
                }
            }
        }
        for (name, c, v) in &ws.state {
            let id = sheet_id(book, name)?;
            let cv = match v {
                Scalar::Empty => CellValue::Empty,
                Scalar::Num(n) => CellValue::Number(*n),
                Scalar::Text(t) => CellValue::Text(t.clone()),
                Scalar::Bool(b) => CellValue::Bool(*b),
                Scalar::Err(e) => CellValue::Error(*e),
            };
            if set_range(id, Rect::single(*c), Some(&[&cv])).is_err() {
                enter_value(id, *c, &cv)?;
            }
        }
        Ok(())
    })();
    let _ = excel(xlcOptionsCalculation, &mut [num(mode)]);
    let _ = excel(xlcEcho, &mut [boolean(true)]);
    status("BatchCalc: recalculating report formulas...");
    let _ = excel(xlcCalculateNow, &mut []);
    result
}
