//! BatchCalc: run a workbook's case loop (what its VBA macro does) natively over all cases.
pub mod contract;
pub mod runner;
pub mod verify;

pub use contract::Contract;
pub use runner::{run, Progress, RunOptions, RunResult};

use std::path::{Path, PathBuf};

/// Find a contract whose required sheets all exist in the workbook (sheet names only).
pub fn find_contract(dirs: &[PathBuf], sheet_names: &[String]) -> Result<(PathBuf, Contract), String> {
    let mut seen = Vec::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
        entries.sort();
        for p in entries {
            let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            let c = Contract::from_json(&text).map_err(|e| format!("{}: {e}", p.display()))?;
            if c.required_sheets.iter().all(|r| sheet_names.iter().any(|s| s.eq_ignore_ascii_case(r))) {
                return Ok((p, c));
            }
            seen.push(c.name);
        }
    }
    Err(if seen.is_empty() { "no batch contracts found".into() } else { format!("no contract matches this workbook (known: {})", seen.join(", ")) })
}

/// Sheet names of a workbook without loading cell data.
pub fn sheet_names(path: &Path) -> Result<Vec<String>, String> {
    let wb = xlmodel::read_workbook(path, &xlmodel::ReadOptions { sheets: Some(vec![]) }).map_err(|e| e.to_string())?;
    Ok(wb.sheets.into_iter().map(|s| s.name).collect())
}

/// Sheets a command clears completely (their stored contents are not needed to run it).
pub fn fully_cleared_sheets(contract: &Contract, stages: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for s in stages {
        if let Some(st) = contract.stage(s) {
            out.extend(st.clear.iter().filter(|c| !c.contains('!')).cloned());
        }
    }
    out
}

pub fn command_stages(contract: &Contract, command: &str) -> Result<Vec<String>, String> {
    if let Some(c) = contract.commands.iter().find(|c| c.name.eq_ignore_ascii_case(command)) {
        return Ok(c.stages.clone());
    }
    if contract.stage(command).is_some() {
        return Ok(vec![command.to_string()]);
    }
    Err(format!("unknown command '{command}' (available: {})", contract.commands.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ")))
}
