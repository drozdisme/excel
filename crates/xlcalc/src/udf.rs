//! Workbook VBA user-defined functions translated to native code.
//! Each translation mirrors the VBA source line by line (see audit/vba_TUCUserDefinedFuncs).
use crate::eval::{unsupported, Ctx, Job, R};
use crate::scalar::Scalar;
use crate::value::{RefRange, Value};
use xlformula::Expr;
use xlmodel::{Cell, Rect};

impl<'m> Job<'m> {
    pub(crate) fn call_udf(&mut self, name: &str, args: &[Expr], cx: &Ctx) -> R<Option<Value>> {
        Ok(Some(match name {
            // TUCSTROFFSET(rng): count rows from rng's first cell down Node_Map until a non-empty
            // cell or row 106.
            "TUCSTROFFSET" => {
                let Value::R(r) = self.eval(&args[0], cx)? else { return Ok(Some(Value::err(xlmodel::XlError::Value))) };
                let Some(ws) = self.m.sheet_index("Node_Map") else { return unsupported("TUCSTROFFSET without Node_Map") };
                let (mut row, col) = (r.rect.r1 + 1, r.rect.c1);
                let mut counter = 1.0;
                while row < 106 && self.vba_is_blank(ws, Cell { row, col })? {
                    counter += 1.0;
                    row += 1;
                }
                Value::num(counter)
            }
            // TUCSHEARTIEOFFSET(rng): extend down while column+1 holds -1 (LONG bays), up to row 119;
            // returns the 5-column range.
            "TUCSHEARTIEOFFSET" => {
                let Value::R(r) = self.eval(&args[0], cx)? else { return Ok(Some(Value::err(xlmodel::XlError::Value))) };
                let (start_row, start_col) = (r.rect.r1, r.rect.c1);
                let mut end_row = start_row;
                while end_row < 119 && self.vba_eq_num(r.sheet, Cell { row: end_row + 1, col: start_col + 1 }, -1.0)? {
                    end_row += 1;
                }
                Value::R(RefRange { sheet: r.sheet, rect: Rect { r1: start_row, c1: start_col, r2: end_row, c2: start_col + 4 } })
            }
            _ => return Ok(None),
        }))
    }

    fn uniform_cell(&mut self, sheet: usize, c: Cell) -> R<Scalar> {
        match self.cell(sheet, c)?.as_uniform() {
            Some(s) => Ok(s.clone()),
            None => unsupported("VBA UDF reading case-dependent cells"),
        }
    }
    /// VBA `cell.Value2 = ""`.
    fn vba_is_blank(&mut self, sheet: usize, c: Cell) -> R<bool> {
        Ok(match self.uniform_cell(sheet, c)? {
            Scalar::Empty => true,
            Scalar::Text(t) => t.is_empty(),
            Scalar::Num(_) | Scalar::Bool(_) => false,
            Scalar::Err(_) => return unsupported("VBA comparison with an error value"),
        })
    }
    /// VBA `cell.Value2 = k` for a numeric constant.
    fn vba_eq_num(&mut self, sheet: usize, c: Cell, k: f64) -> R<bool> {
        Ok(match self.uniform_cell(sheet, c)? {
            Scalar::Num(x) => x == k,
            Scalar::Empty => k == 0.0,
            Scalar::Bool(b) => (if b { -1.0 } else { 0.0 }) == k,
            Scalar::Text(t) => crate::scalar::parse_number(&t) == Some(k),
            Scalar::Err(_) => return unsupported("VBA comparison with an error value"),
        })
    }
}
