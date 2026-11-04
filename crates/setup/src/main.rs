//! BatchCalc setup: per-user install (no administrator), Excel add-in registration,
//! "Apps & features" entry and clean uninstall. Usage:
//!   BatchCalc-Setup.exe              interactive install / update
//!   BatchCalc-Setup.exe /quiet       silent install
//!   BatchCalc-Setup.exe /uninstall   remove (add /quiet for silent)
#![windows_subsystem = "windows"]
mod reg;

use reg::{Key, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use std::path::{Path, PathBuf};

include!(concat!(env!("OUT_DIR"), "/payload.rs"));

const APP: &str = "BatchCalc";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const XLL: &str = "BatchCalc.xll";

/// BATCHCALC_TEST_ROOT redirects every registry key and the folder (for automated tests).
fn test_root() -> Option<String> {
    std::env::var("BATCHCALC_TEST_ROOT").ok().filter(|s| !s.is_empty())
}
fn uninstall_key() -> String {
    match test_root() {
        Some(r) => format!(r"Software\BatchCalcTest\{r}\Uninstall\BatchCalc"),
        None => r"Software\Microsoft\Windows\CurrentVersion\Uninstall\BatchCalc".into(),
    }
}
fn excel_options() -> String {
    match test_root() {
        Some(r) => format!(r"Software\BatchCalcTest\{r}\Excel\Options"),
        None => r"Software\Microsoft\Office\16.0\Excel\Options".into(),
    }
}
fn install_dir() -> PathBuf {
    match test_root() {
        Some(r) => std::env::temp_dir().join("BatchCalcTest").join(r),
        None => PathBuf::from(std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA")).join("Programs").join(APP),
    }
}

#[link(name = "user32")]
unsafe extern "system" {
    fn MessageBoxW(hwnd: isize, text: *const u16, caption: *const u16, kind: u32) -> i32;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

struct Ui {
    quiet: bool,
}
impl Ui {
    fn show(&self, msg: &str, kind: u32) -> i32 {
        unsafe { MessageBoxW(0, wide(msg).as_ptr(), wide(APP).as_ptr(), kind) }
    }
    fn info(&self, msg: &str) {
        if !self.quiet {
            self.show(msg, 0x40);
        }
    }
    fn error(&self, msg: &str) {
        if !self.quiet {
            self.show(msg, 0x10);
        }
    }
    /// OK/Cancel question; quiet mode answers OK.
    fn ask(&self, msg: &str) -> bool {
        self.quiet || self.show(msg, 0x21) == 1
    }
    /// Retry/Cancel; quiet mode gives up.
    fn retry(&self, msg: &str) -> bool {
        !self.quiet && self.show(msg, 0x35) == 4
    }
}

fn excel_bitness() -> Option<String> {
    for (path, value) in [
        (r"SOFTWARE\Microsoft\Office\ClickToRun\Configuration", "Platform"),
        (r"SOFTWARE\Microsoft\Office\16.0\Outlook", "Bitness"),
    ] {
        if let Some(v) = Key::open(HKEY_LOCAL_MACHINE, path, false).and_then(|k| k.get_str(value)) {
            return Some(v);
        }
    }
    None
}

fn open_name(i: usize) -> String {
    if i == 0 { "OPEN".to_string() } else { format!("OPEN{i}") }
}

/// OPEN, OPEN1, OPEN2 ... values; Excel stops reading at the first gap.
fn open_values(opts: &Key) -> Vec<String> {
    (0..).map_while(|i| opts.get_str(&open_name(i))).collect()
}

fn write_open_values(opts: &Key, old_len: usize, vals: &[String]) -> Result<(), String> {
    for i in 0..old_len.max(vals.len()) {
        match vals.get(i) {
            Some(v) => opts.set_str(&open_name(i), v)?,
            None => opts.delete_value(&open_name(i)),
        }
    }
    Ok(())
}

fn is_ours(v: &str) -> bool {
    v.to_ascii_lowercase().contains(&XLL.to_ascii_lowercase())
}

fn install(ui: &Ui) -> Result<(), String> {
    if let Some(b) = excel_bitness()
        && b.contains("86") {
            return Err(format!("BatchCalc needs 64-bit Excel; this computer has {b} Office."));
        }
    let dir = install_dir();
    if !ui.ask(&format!("Install {APP} {VERSION} for the current user?\n\nFolder: {}\nExcel: adds the BatchCalc menu (Add-ins tab).", dir.display())) {
        return Ok(());
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // Files (the add-in is locked while Excel has it loaded).
    for (rel, data) in FILES {
        let target = dir.join(rel);
        if let Some(p) = target.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        while let Err(e) = std::fs::write(&target, data) {
            if !ui.retry(&format!("Cannot update {rel} ({e}).\n\nClose all Excel windows and press Retry.")) {
                return Err(format!("{rel}: {e}"));
            }
        }
    }
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let setup_copy = dir.join("BatchCalc-Setup.exe");
    if me != setup_copy {
        std::fs::copy(&me, &setup_copy).map_err(|e| format!("{}: {e}", setup_copy.display()))?;
    }
    // Excel loads the add-in at start for this user.
    let opts = Key::create(HKEY_CURRENT_USER, &excel_options())?;
    let mut vals = open_values(&opts);
    let old = vals.len();
    vals.retain(|v| !is_ours(v));
    vals.push(format!("\"{}\"", dir.join(XLL).display()));
    write_open_values(&opts, old, &vals)?;
    // Apps & features entry.
    let u = Key::create(HKEY_CURRENT_USER, &uninstall_key())?;
    u.set_str("DisplayName", APP)?;
    u.set_str("DisplayVersion", VERSION)?;
    u.set_str("Publisher", APP)?;
    u.set_str("InstallLocation", &dir.display().to_string())?;
    u.set_str("DisplayIcon", &setup_copy.display().to_string())?;
    u.set_str("UninstallString", &format!("\"{}\" /uninstall", setup_copy.display()))?;
    u.set_str("QuietUninstallString", &format!("\"{}\" /uninstall /quiet", setup_copy.display()))?;
    u.set_dword("NoModify", 1)?;
    u.set_dword("NoRepair", 1)?;
    u.set_dword("EstimatedSize", (FILES.iter().map(|(_, d)| d.len()).sum::<usize>() / 1024) as u32)?;
    ui.info(&format!("{APP} {VERSION} is installed.\n\nRestart Excel, open your workbook and use Add-ins > BatchCalc.\nTo remove it: Settings > Apps > {APP}."));
    Ok(())
}

fn uninstall(ui: &Ui) -> Result<(), String> {
    if !ui.ask(&format!("Remove {APP} from this computer?")) {
        return Ok(());
    }
    if let Some(opts) = Key::open(HKEY_CURRENT_USER, &excel_options(), true) {
        let vals = open_values(&opts);
        let keep: Vec<String> = vals.iter().filter(|v| !is_ours(v)).cloned().collect();
        write_open_values(&opts, vals.len(), &keep)?;
    }
    reg::delete_tree(HKEY_CURRENT_USER, &uninstall_key());
    let dir = install_dir();
    for (rel, _) in FILES {
        let p = dir.join(rel);
        while p.exists() && std::fs::remove_file(&p).is_err() {
            if !ui.retry(&format!("Cannot remove {rel}. Close all Excel windows and press Retry.")) {
                return Err(format!("could not remove {}", p.display()));
            }
        }
    }
    // We run from a temporary copy (see main), so the installed setup can be removed; the
    // launching process may still be exiting, hence the short retry.
    let setup_copy = dir.join("BatchCalc-Setup.exe");
    for _ in 0..50 {
        if !setup_copy.exists() || std::fs::remove_file(&setup_copy).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    remove_empty_dirs(&dir);
    let _ = std::fs::remove_dir(&dir);
    ui.info(&format!("{APP} has been removed. Restart Excel to unload the add-in."));
    Ok(())
}

fn remove_empty_dirs(dir: &Path) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                remove_empty_dirs(&e.path());
                let _ = std::fs::remove_dir(e.path());
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).map(|a| a.to_ascii_lowercase()).collect();
    let ui = Ui { quiet: args.iter().any(|a| a == "/quiet" || a == "/q" || a == "--quiet") };
    let uninstalling = args.iter().any(|a| a == "/uninstall" || a == "--uninstall");
    // An installed setup cannot delete itself: relaunch from a temporary copy and exit.
    if uninstalling {
        if let Ok(me) = std::env::current_exe() {
            if me.starts_with(install_dir()) {
                let tmp = std::env::temp_dir().join(format!("BatchCalc-Uninstall-{}.exe", std::process::id()));
                if std::fs::copy(&me, &tmp).is_ok() && std::process::Command::new(&tmp).args(std::env::args().skip(1)).spawn().is_ok() {
                    return;
                }
            }
        }
    }
    let res = if uninstalling { uninstall(&ui) } else { install(&ui) };
    if let Err(e) = res {
        ui.error(&e);
        std::process::exit(1);
    }
}
