//! Streaming reader of an Excel OOXML workbook (xlsx/xlsm) into an immutable model:
//! typed cell values, formulas (shared/array groups kept compact), defined names.

mod reader;
pub mod value;

pub use reader::{read_workbook, ReadOptions};
pub use value::{CellValue, XlError};

use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("xml in {part}: {msg}")]
    Xml { part: String, msg: String },
    #[error("workbook structure: {0}")]
    Structure(String),
}

/// 1-based row/column coordinate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Cell {
    pub row: u32,
    pub col: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rect {
    pub r1: u32,
    pub c1: u32,
    pub r2: u32,
    pub c2: u32,
}

impl Rect {
    pub fn single(c: Cell) -> Self {
        Rect { r1: c.row, c1: c.col, r2: c.row, c2: c.col }
    }
    pub fn rows(&self) -> u32 {
        self.r2 - self.r1 + 1
    }
    pub fn cols(&self) -> u32 {
        self.c2 - self.c1 + 1
    }
    pub fn contains(&self, c: Cell) -> bool {
        (self.r1..=self.r2).contains(&c.row) && (self.c1..=self.c2).contains(&c.col)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormulaKind {
    /// Ordinary cell formula.
    Normal,
    /// Legacy CSE or dynamic array formula anchored here, occupying `range`.
    Array { range: Rect, dynamic: bool },
}

/// A formula as stored. Shared-group members point to the master text and origin, so
/// the formula layer can parse each group once and relocate relative references.
#[derive(Clone, Debug)]
pub struct Formula {
    /// A1 text without the leading '=' exactly as written at `origin`.
    pub text: Arc<str>,
    /// Cell in which `text` was written (differs from the cell for shared members).
    pub origin: Cell,
    pub kind: FormulaKind,
}

#[derive(Default, Debug)]
pub struct Sheet {
    pub name: String,
    pub hidden: bool,
    /// Dense rows: rows[r-1][c-1]. Rows are trimmed to their last stored cell.
    pub rows: Vec<Vec<CellValue>>,
    pub formulas: HashMap<Cell, Formula>,
    /// Spill/array member -> anchor cell (anchor included).
    pub array_members: HashMap<Cell, Cell>,
}

impl Sheet {
    pub fn value(&self, c: Cell) -> &CellValue {
        static EMPTY: CellValue = CellValue::Empty;
        self.rows
            .get(c.row as usize - 1)
            .and_then(|r| r.get(c.col as usize - 1))
            .unwrap_or(&EMPTY)
    }
    pub fn max_row(&self) -> u32 {
        self.rows.len() as u32
    }
    pub fn max_col(&self) -> u32 {
        self.rows.iter().map(|r| r.len()).max().unwrap_or(0) as u32
    }
    pub(crate) fn set(&mut self, c: Cell, v: CellValue) {
        let (r, col) = (c.row as usize - 1, c.col as usize - 1);
        if self.rows.len() <= r {
            self.rows.resize_with(r + 1, Vec::new);
        }
        let row = &mut self.rows[r];
        if row.len() <= col {
            row.resize(col + 1, CellValue::Empty);
        }
        row[col] = v;
    }
}

#[derive(Clone, Debug)]
pub struct DefinedName {
    pub name: String,
    /// Index into `Workbook::sheets` when sheet-scoped.
    pub scope: Option<usize>,
    /// Formula text without '='.
    pub refers_to: String,
    pub hidden: bool,
}

#[derive(Default, Debug)]
pub struct Workbook {
    pub sheets: Vec<Sheet>,
    pub names: Vec<DefinedName>,
    pub date1904: bool,
}

impl Workbook {
    pub fn sheet_index(&self, name: &str) -> Option<usize> {
        self.sheets.iter().position(|s| s.name.eq_ignore_ascii_case(name))
    }
    pub fn sheet(&self, name: &str) -> Option<&Sheet> {
        self.sheet_index(name).map(|i| &self.sheets[i])
    }
    /// Sheet-scoped name wins over a workbook-scoped one, as in Excel.
    pub fn find_name(&self, name: &str, from_sheet: Option<usize>) -> Option<&DefinedName> {
        let mut global = None;
        for n in &self.names {
            if n.name.eq_ignore_ascii_case(name) {
                match n.scope {
                    Some(s) if Some(s) == from_sheet => return Some(n),
                    None => global = Some(n),
                    _ => {}
                }
            }
        }
        global
    }
    pub fn formula_count(&self) -> usize {
        self.sheets.iter().map(|s| s.formulas.len()).sum()
    }
}

/// Parse "AB12" (with optional '$') into a cell.
pub fn parse_a1(s: &str) -> Option<Cell> {
    let b = s.as_bytes();
    let mut i = 0;
    if b.get(i) == Some(&b'$') {
        i += 1;
    }
    let mut col: u32 = 0;
    let start = i;
    while i < b.len() && b[i].is_ascii_alphabetic() {
        col = col * 26 + (b[i].to_ascii_uppercase() - b'A' + 1) as u32;
        i += 1;
        if i - start > 3 {
            return None;
        }
    }
    if i == start {
        return None;
    }
    if b.get(i) == Some(&b'$') {
        i += 1;
    }
    let digits = &s[i..];
    if digits.is_empty() || !digits.bytes().all(|d| d.is_ascii_digit()) {
        return None;
    }
    let row: u32 = digits.parse().ok()?;
    if row == 0 || row > 1_048_576 || col > 16_384 {
        return None;
    }
    Some(Cell { row, col })
}

pub fn parse_rect(s: &str) -> Option<Rect> {
    match s.split_once(':') {
        Some((a, b)) => {
            let (a, b) = (parse_a1(a)?, parse_a1(b)?);
            Some(Rect { r1: a.row.min(b.row), c1: a.col.min(b.col), r2: a.row.max(b.row), c2: a.col.max(b.col) })
        }
        None => parse_a1(s).map(Rect::single),
    }
}

pub fn col_name(mut col: u32) -> String {
    let mut out = Vec::new();
    while col > 0 {
        let rem = (col - 1) % 26;
        out.push(b'A' + rem as u8);
        col = (col - 1) / 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}

pub fn a1(c: Cell) -> String {
    format!("{}{}", col_name(c.col), c.row)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a1_roundtrip() {
        for (s, r, c) in [("A1", 1, 1), ("$Z$9", 9, 26), ("AA10", 10, 27), ("XFD1048576", 1048576, 16384)] {
            let cell = parse_a1(s).unwrap();
            assert_eq!((cell.row, cell.col), (r, c));
            assert_eq!(a1(cell), s.replace('$', ""));
        }
        assert!(parse_a1("A0").is_none());
        assert!(parse_a1("ABCD1").is_none());
        assert!(parse_a1("1A").is_none());
    }
}
