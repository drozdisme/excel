//! Compare a run's output sheets with the values stored in a workbook (e.g. the results the
//! original macro saved), cell by cell, including headers, text, errors and blanks.
use crate::runner::RunResult;
use xlmodel::{Cell, CellValue, Workbook};

#[derive(Debug, Default)]
pub struct SheetDiff {
    pub sheet: String,
    pub compared: usize,
    pub different: usize,
    pub max_abs: f64,
    pub examples: Vec<String>,
}

/// Errors newer than the OOXML error set are stored in files as #VALUE!.
pub fn file_error(e: xlmodel::XlError) -> xlmodel::XlError {
    use xlmodel::XlError::*;
    match e {
        Null | Div0 | Value | Ref | Name | Num | NA | GettingData => e,
        _ => Value,
    }
}

pub fn equal(a: &CellValue, b: &CellValue, max_abs: &mut f64) -> bool {
    match (a, b) {
        (CellValue::Error(x), CellValue::Error(y)) => file_error(*x) == file_error(*y),
        (CellValue::Number(x), CellValue::Number(y)) => {
            let d = (x - y).abs();
            *max_abs = max_abs.max(d);
            d <= 1e-9 + 1e-9 * y.abs()
        }
        _ => a == b,
    }
}

pub fn compare(result: &RunResult, golden: &Workbook) -> Vec<SheetDiff> {
    let mut out = Vec::new();
    let mut sheets: Vec<&usize> = result.patches.keys().collect();
    sheets.sort();
    for &s in sheets {
        let p = &result.patches[&s];
        let g = &golden.sheets[s];
        let mut d = SheetDiff { sheet: g.name.clone(), ..Default::default() };
        let mut check = |c: Cell, ours: &CellValue| {
            d.compared += 1;
            let theirs = g.value(c);
            if !equal(ours, theirs, &mut d.max_abs) {
                d.different += 1;
                if d.examples.len() < 5 {
                    d.examples.push(format!("{}!{}: engine {:?}, workbook {:?}", g.name, xlmodel::a1(c), ours, theirs));
                }
            }
        };
        // Every cell we wrote.
        for (ri, line) in p.rows.iter().enumerate() {
            for (ci, v) in line.iter().enumerate() {
                if let Some(v) = v {
                    check(Cell { row: ri as u32 + 1, col: ci as u32 + 1 }, v);
                }
            }
        }
        // Every non-empty stored cell we cleared and did not write must be empty in the workbook too.
        let empty = CellValue::Empty;
        for (ri, line) in g.rows.iter().enumerate() {
            for (ci, v) in line.iter().enumerate() {
                let c = Cell { row: ri as u32 + 1, col: ci as u32 + 1 };
                if *v != CellValue::Empty && p.written(c).is_none() && p.is_cleared(c) {
                    check(c, &empty);
                }
            }
        }
        out.push(d);
    }
    out
}
