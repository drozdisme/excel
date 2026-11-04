//! batchcalc CLI: run a workbook's batch calculation natively and optionally verify it against
//! the results stored in the workbook.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use batchcalc::{command_stages, find_contract, fully_cleared_sheets, sheet_names, verify, Contract, RunOptions};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

const USAGE: &str = "usage: batchcalc run WORKBOOK [--command NAME] [--contract FILE] [--threads N] [--cases N] [--verify]
       batchcalc commands WORKBOOK [--contract FILE]";

fn contract_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            v.push(d.join("contracts"));
            v.push(d.join("..").join("..").join("contracts"));
        }
    }
    v.push(PathBuf::from("contracts"));
    v
}

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("batchcalc: {msg}");
    std::process::exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(cmd), Some(book)) = (args.first(), args.get(1)) else { fail(USAGE) };
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let book = PathBuf::from(book);
    let names = sheet_names(&book).unwrap_or_else(|e| fail(e));
    let (cpath, contract): (PathBuf, Contract) = match flag("--contract") {
        Some(p) => {
            let text = std::fs::read_to_string(&p).unwrap_or_else(|e| fail(format!("{p}: {e}")));
            (PathBuf::from(&p), Contract::from_json(&text).unwrap_or_else(|e| fail(e)))
        }
        None => find_contract(&contract_dirs(), &names).unwrap_or_else(|e| fail(e)),
    };
    match cmd.as_str() {
        "commands" => {
            println!("{} ({})", contract.name, cpath.display());
            for c in &contract.commands {
                println!("  {:<14} {}  [{}]", c.name, c.label, c.stages.join(" -> "));
            }
        }
        "run" => {
            let command = flag("--command").unwrap_or_else(|| "all".into());
            let stages = command_stages(&contract, &command).unwrap_or_else(|e| fail(e));
            let verify_mode = args.iter().any(|a| a == "--verify");
            let t0 = Instant::now();
            // Sheets the command clears completely are not read (unless verifying against them).
            let skip = if verify_mode { vec![] } else { fully_cleared_sheets(&contract, &stages) };
            let load: Vec<String> = names.iter().filter(|n| !skip.iter().any(|s| s.eq_ignore_ascii_case(n))).cloned().collect();
            let wb = xlmodel::read_workbook(&book, &xlmodel::ReadOptions { sheets: Some(load) }).unwrap_or_else(|e| fail(e));
            let t_read = t0.elapsed().as_secs_f64();
            let model = xlcalc::Model::new(wb).unwrap_or_else(|e| fail(e));
            println!("{}: {} -> {} | read {:.2}s, model {:.2}s", contract.name, command, stages.join(" -> "), t_read, t0.elapsed().as_secs_f64() - t_read);
            let mut opts = RunOptions::default();
            if let Some(t) = flag("--threads") {
                opts.threads = t.parse().unwrap_or_else(|_| fail("bad --threads"));
            }
            opts.case_limit = flag("--cases").map(|c| c.parse().unwrap_or_else(|_| fail("bad --cases")));
            opts.progress = Some(Arc::new(|p: &batchcalc::Progress| {
                if p.done == p.total || p.done % 20 == 0 {
                    eprint!("\r  [{}/{}] {:<12} {:>4}/{:<4}", p.stage_index + 1, p.stage_count, p.stage, p.done, p.total);
                    if p.done == p.total {
                        eprintln!();
                    }
                }
            }));
            let t1 = Instant::now();
            let result = batchcalc::run(&model, &contract, &stages, &opts).unwrap_or_else(|e| fail(e));
            let t_calc = t1.elapsed().as_secs_f64();
            for r in &result.reports {
                println!(
                    "  stage {:<12} {:>4} jobs ({} with cases), {} cases, {:>9} cells written, {} Excel error values, {:.2}s",
                    r.name, r.jobs, r.active_jobs, r.cases, r.cells_written, r.excel_errors, r.seconds
                );
            }
            println!("calculation {:.2}s, total {:.2}s, {} threads", t_calc, t0.elapsed().as_secs_f64(), opts.threads);
            if verify_mode {
                let diffs = verify::compare(&result, &model.wb);
                let (mut cmp, mut bad) = (0, 0);
                for d in &diffs {
                    cmp += d.compared;
                    bad += d.different;
                    println!("  verify {:<20} {:>9} cells, {:>6} different, max |diff| {:.3e}", d.sheet, d.compared, d.different, d.max_abs);
                    for e in &d.examples {
                        println!("      {e}");
                    }
                }
                println!("VERIFY {}: {cmp} cells compared, {bad} different", if bad == 0 { "PASS" } else { "FAIL" });
                if bad > 0 {
                    std::process::exit(3);
                }
            }
        }
        "diff" => {
            // diff A B --sheets S1,S2: compare stored values of two workbooks cell by cell.
            let other = args.get(2).cloned().unwrap_or_else(|| fail("diff needs two workbooks"));
            let sheets: Vec<String> = flag("--sheets").map(|s| s.split(',').map(str::to_string).collect()).unwrap_or_else(|| fail("diff needs --sheets"));
            let opts = xlmodel::ReadOptions { sheets: Some(sheets.clone()) };
            let a = xlmodel::read_workbook(&book, &opts).unwrap_or_else(|e| fail(e));
            let b = xlmodel::read_workbook(other.as_ref(), &opts).unwrap_or_else(|e| fail(e));
            let mut total_bad = 0;
            for s in &sheets {
                let (Some(x), Some(y)) = (a.sheet(s), b.sheet(s)) else { fail(format!("sheet {s} missing")) };
                let (rows, cols) = (x.max_row().max(y.max_row()), x.max_col().max(y.max_col()));
                let (mut n, mut bad, mut max) = (0usize, 0usize, 0f64);
                for row in 1..=rows {
                    for col in 1..=cols {
                        let c = xlmodel::Cell { row, col };
                        let (p, q) = (x.value(c), y.value(c));
                        if *p == xlmodel::CellValue::Empty && *q == xlmodel::CellValue::Empty {
                            continue;
                        }
                        n += 1;
                        if !verify::equal(p, q, &mut max) {
                            bad += 1;
                            if bad <= 3 {
                                println!("    {s}!{}: {:?} vs {:?}", xlmodel::a1(c), p, q);
                            }
                        }
                    }
                }
                total_bad += bad;
                println!("  {s:<20} {n:>9} cells, {bad} different, max |diff| {max:.3e}");
            }
            println!("DIFF {}", if total_bad == 0 { "EQUAL" } else { "DIFFERENT" });
            if total_bad > 0 {
                std::process::exit(3);
            }
        }
        "selfcheck" => {
            // Recalculate every formula of the given sheets with the workbook's own inputs and
            // compare with the values Excel stored. Diagnoses semantic differences.
            let sheets: Vec<String> = flag("--sheets").map(|s| s.split(',').map(str::to_string).collect()).unwrap_or_default();
            let wb = xlmodel::read_workbook(&book, &Default::default()).unwrap_or_else(|e| fail(e));
            let model = xlcalc::Model::new(wb).unwrap_or_else(|e| fail(e));
            let max: usize = flag("--max").and_then(|m| m.parse().ok()).unwrap_or(40);
            // --set Sheet!A1=value (repeatable): driver assignments, typed like Excel entry.
            let sets: Vec<(usize, xlmodel::Cell, xlcalc::Scalar)> = args
                .windows(2)
                .filter(|w| w[0] == "--set")
                .map(|w| {
                    let (addr, val) = w[1].split_once('=').unwrap_or_else(|| fail("--set needs Sheet!A1=value"));
                    let (s, r) = batchcalc::runner::resolve_range(&model, addr).unwrap_or_else(|e| fail(e));
                    let v = match val.parse::<f64>() {
                        Ok(n) => xlcalc::Scalar::Num(n),
                        Err(_) => xlcalc::Scalar::text(val.trim_start_matches('\'')),
                    };
                    (s, xlmodel::Cell { row: r.r1, col: r.c1 }, v)
                })
                .collect();
            // --oracle file.tsv from tools/Capture-Oracle.ps1 replaces stored values as reference.
            let oracle: Option<std::collections::HashMap<(String, u32, u32), xlmodel::CellValue>> = flag("--oracle").map(|p| {
                let text = std::fs::read_to_string(&p).unwrap_or_else(|e| fail(format!("{p}: {e}")));
                text.lines()
                    .skip(1)
                    .filter_map(|l| {
                        let f: Vec<&str> = l.splitn(5, '\t').collect();
                        let v = match f[3] {
                            "n" => xlmodel::CellValue::Number(f[4].parse().ok()?),
                            "s" => xlmodel::CellValue::Text(f[4].into()),
                            "b" => xlmodel::CellValue::Bool(f[4] == "True"),
                            "e" => xlmodel::CellValue::Error(match f[4] {
                                "-2146826281" => xlmodel::XlError::Div0,
                                "-2146826246" => xlmodel::XlError::NA,
                                "-2146826259" => xlmodel::XlError::Name,
                                "-2146826288" => xlmodel::XlError::Null,
                                "-2146826252" => xlmodel::XlError::Num,
                                "-2146826265" => xlmodel::XlError::Ref,
                                _ => xlmodel::XlError::Value,
                            }),
                            _ => return None,
                        };
                        Some(((f[0].to_ascii_lowercase(), f[1].parse().ok()?, f[2].parse().ok()?), v))
                    })
                    .collect()
            });
            let explain = flag("--explain");
            let handle = std::thread::Builder::new()
                .stack_size(1 << 30)
                .spawn_scoped_compat(move || {
                    let mut job = xlcalc::Job::new(&model, 1);
                    for (s, c, v) in &sets {
                        job.set_override(*s, *c, xlcalc::Lanes::Uniform(v.clone()));
                    }
                    let (mut ok, mut bad, mut errs) = (0, 0, 0);
                    for (si, sh) in model.wb.sheets.iter().enumerate() {
                        if !sheets.is_empty() && !sheets.iter().any(|s| s.eq_ignore_ascii_case(&sh.name)) {
                            continue;
                        }
                        let mut cells: Vec<_> = sh.formulas.keys().copied().chain(sh.array_members.keys().copied()).collect();
                        cells.sort_by_key(|c| (c.row, c.col));
                        cells.dedup();
                        for c in cells {
                            let empty = xlmodel::CellValue::Empty;
                            let stored = match &oracle {
                                Some(o) => o.get(&(sh.name.to_ascii_lowercase(), c.row, c.col)).unwrap_or(&empty),
                                None => sh.value(c),
                            };
                            match job.cell(si, c) {
                                Ok(l) => {
                                    let v = l.get(0);
                                    let ours = match &v {
                                        xlcalc::Scalar::Empty => xlmodel::CellValue::Empty,
                                        xlcalc::Scalar::Num(n) => xlmodel::CellValue::Number(*n),
                                        xlcalc::Scalar::Text(t) => xlmodel::CellValue::Text(t.clone()),
                                        xlcalc::Scalar::Bool(b) => xlmodel::CellValue::Bool(*b),
                                        xlcalc::Scalar::Err(e) => xlmodel::CellValue::Error(*e),
                                    };
                                    let mut m = 0.0;
                                    if verify::equal(&ours, stored, &mut m) || (ours == xlmodel::CellValue::Number(0.0) && *stored == xlmodel::CellValue::Empty) {
                                        ok += 1;
                                    } else {
                                        bad += 1;
                                        if bad <= max {
                                            let f = sh.formulas.get(&c).map(|f| f.text.to_string()).unwrap_or_else(|| "(array member)".into());
                                            println!("{}!{}: engine {:?} | Excel {:?}\n    ={}", sh.name, xlmodel::a1(c), ours, stored, f);
                                        }
                                    }
                                }
                                Err(e) => {
                                    errs += 1;
                                    if errs <= 10 {
                                        println!("{}!{}: ERROR {e}", sh.name, xlmodel::a1(c));
                                    }
                                }
                            }
                        }
                    }
                    println!("selfcheck: {ok} equal, {bad} different, {errs} engine errors");
                    // --explain Sheet!A1: formula as seen from the cell and its direct references.
                    if let Some(target) = &explain {
                        let (si, r) = batchcalc::runner::resolve_range(&model, target).unwrap_or_else(|e| fail(e));
                        let c = xlmodel::Cell { row: r.r1, col: r.c1 };
                        let sh = &model.wb.sheets[si];
                        let empty = xlmodel::CellValue::Empty;
                        let reference = |sheet: &str, rr: u32, cc: u32| -> xlmodel::CellValue {
                            match &oracle {
                                Some(o) => o.get(&(sheet.to_ascii_lowercase(), rr, cc)).cloned().unwrap_or(empty.clone()),
                                None => model.wb.sheet(sheet).map(|s| s.value(xlmodel::Cell { row: rr, col: cc }).clone()).unwrap_or(empty.clone()),
                            }
                        };
                        match model.formula_expr(si, c) {
                            None => println!("{target}: no formula (array member of {:?})", sh.array_members.get(&c).map(|a| xlmodel::a1(*a))),
                            Some(e) => {
                                println!("{target} = {}", xlformula::to_a1(&e, c));
                                println!("  engine {:?} | reference {:?}", job.cell(si, c).map(|l| l.get(0)), reference(&sh.name, c.row, c.col));
                                let mut refs = Vec::new();
                                xlformula::references(&e, &mut refs);
                                for rf in refs {
                                    let rs = rf.sheet.as_deref().and_then(|n| model.sheet_index(n)).unwrap_or(si);
                                    let row = |k: xlformula::Coord| k.at(c.row, xlformula::MAX_ROW).unwrap_or(1);
                                    let col = |k: xlformula::Coord| k.at(c.col, xlformula::MAX_COL).unwrap_or(1);
                                    let (r1, c1, r2, c2) = match rf.area {
                                        xlformula::Area::Cell { row: a, col: b } => (row(a), col(b), row(a), col(b)),
                                        xlformula::Area::Range { r1, c1, r2, c2 } => (row(r1), col(c1), row(r2), col(c2)),
                                        _ => continue,
                                    };
                                    for rr in r1..=r2.min(r1 + 30) {
                                        for cc in c1..=c2.min(c1 + 10) {
                                            let cell = xlmodel::Cell { row: rr, col: cc };
                                            let ours = job.cell(rs, cell).map(|l| l.get(0));
                                            let name = &model.wb.sheets[rs].name;
                                            println!("    {}!{}: engine {:?} | reference {:?}", name, xlmodel::a1(cell), ours, reference(name, rr, cc));
                                        }
                                    }
                                }
                            }
                        }
                    }
                });
            handle.join().unwrap();
        }
        _ => fail(USAGE),
    }
}

trait SpawnCompat {
    fn spawn_scoped_compat<F: FnOnce() + Send + 'static>(self, f: F) -> std::thread::JoinHandle<()>;
}
impl SpawnCompat for std::thread::Builder {
    fn spawn_scoped_compat<F: FnOnce() + Send + 'static>(self, f: F) -> std::thread::JoinHandle<()> {
        self.spawn(f).expect("spawn")
    }
}

