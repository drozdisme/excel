use crate::lanes::Lanes;
use crate::scalar::Scalar;
use std::sync::Arc;
use xlmodel::{Cell, Rect};

/// A rectangular reference on one sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RefRange {
    pub sheet: usize,
    pub rect: Rect,
}

impl RefRange {
    pub fn cell(sheet: usize, c: Cell) -> Self {
        RefRange { sheet, rect: Rect::single(c) }
    }
    pub fn is_cell(&self) -> bool {
        self.rect.r1 == self.rect.r2 && self.rect.c1 == self.rect.c2
    }
    pub fn top_left(&self) -> Cell {
        Cell { row: self.rect.r1, col: self.rect.c1 }
    }
}

/// A 2-D array whose elements each carry all lanes. Shape is shared by every lane.
#[derive(Clone, Debug)]
pub struct Array {
    pub rows: usize,
    pub cols: usize,
    pub data: Vec<Lanes>,
}

impl Array {
    pub fn new(rows: usize, cols: usize, data: Vec<Lanes>) -> Self {
        debug_assert_eq!(rows * cols, data.len());
        Array { rows, cols, data }
    }
    pub fn scalar(l: Lanes) -> Self {
        Array { rows: 1, cols: 1, data: vec![l] }
    }
    pub fn get(&self, r: usize, c: usize) -> &Lanes {
        &self.data[r * self.cols + c]
    }
    /// Excel broadcasting: a single row/column (or 1x1) stretches; other mismatches give #N/A.
    pub fn at_broadcast(&self, r: usize, c: usize) -> Lanes {
        let rr = if self.rows == 1 { 0 } else { r };
        let cc = if self.cols == 1 { 0 } else { c };
        if rr < self.rows && cc < self.cols {
            self.data[rr * self.cols + cc].clone()
        } else {
            Lanes::Uniform(Scalar::Err(xlmodel::XlError::NA))
        }
    }
    pub fn all_uniform(&self) -> bool {
        self.data.iter().all(Lanes::is_uniform)
    }
}

#[derive(Clone, Debug)]
pub enum Value {
    /// One scalar per lane.
    S(Lanes),
    /// Computed array (same shape in every lane).
    A(Arc<Array>),
    /// Reference, identical in every lane.
    R(RefRange),
    /// Reference that differs per lane (e.g. INDEX/OFFSET with case-dependent offsets);
    /// a lane may instead hold the error the reference expression produced.
    LR(Arc<Vec<Result<RefRange, xlmodel::XlError>>>),
}

impl Value {
    pub fn err(e: xlmodel::XlError) -> Self {
        Value::S(Lanes::Uniform(Scalar::Err(e)))
    }
    pub fn num(n: f64) -> Self {
        Value::S(Lanes::Uniform(Scalar::num(n)))
    }
    pub fn text(t: &str) -> Self {
        Value::S(Lanes::Uniform(Scalar::text(t)))
    }
}
