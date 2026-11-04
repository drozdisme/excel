use std::sync::Arc;
use xlmodel::XlError;

/// One coordinate of a reference: absolute (`$A`) or relative to the formula's origin cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Coord {
    Abs(u32),
    /// Offset from the origin cell (A1 text written at the origin).
    Rel(i32),
}

impl Coord {
    /// Resolve at `base` (the evaluating cell's row/col); `None` if outside the sheet.
    pub fn at(self, base: u32, max: u32) -> Option<u32> {
        match self {
            Coord::Abs(v) => Some(v),
            Coord::Rel(d) => {
                let v = base as i64 + d as i64;
                (1..=max as i64).contains(&v).then_some(v as u32)
            }
        }
    }
}

pub const MAX_ROW: u32 = 1_048_576;
pub const MAX_COL: u32 = 16_384;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Area {
    Cell { row: Coord, col: Coord },
    /// Rectangle between two corner cells.
    Range { r1: Coord, c1: Coord, r2: Coord, c2: Coord },
    /// Whole columns, e.g. A:C.
    Cols { c1: Coord, c2: Coord },
    /// Whole rows, e.g. 3:5.
    Rows { r1: Coord, r2: Coord },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Reference {
    /// Sheet name as written; `None` means the formula's own sheet.
    pub sheet: Option<Arc<str>>,
    pub area: Area,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Concat,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// Reference range operator ':' between general expressions.
    Range,
    /// Reference intersection (space).
    Intersect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnOp {
    Neg,
    Plus,
    Percent,
    /// '@' implicit intersection.
    Implicit,
    /// '#' spill reference.
    Spill,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Number(f64),
    Text(Arc<str>),
    Bool(bool),
    Error(XlError),
    /// Omitted argument, e.g. the second one in `IF(A1,,2)`.
    Missing,
    Array(Vec<Vec<Expr>>),
    Ref(Reference),
    /// `#REF!` written in reference position (e.g. `Sheet1!#REF!`).
    RefError,
    /// Defined name, LET variable or LAMBDA parameter (upper-cased, `_xlpm.` stripped).
    Name(Arc<str>),
    /// Function call; name upper-cased with `_xlfn.`/`_xlws.` prefixes stripped.
    Call(Arc<str>, Vec<Expr>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
}
