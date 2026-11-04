//! Excel formula grammar: lexer, Pratt parser and an AST whose relative references are
//! stored as offsets, so one parsed shared/filled formula serves every cell of its group.

pub mod ast;
mod lexer;
mod parser;
mod unparse;

pub use ast::*;
pub use unparse::{references, to_a1, to_r1c1};
pub use parser::{normalize_fn, normalize_name, parse};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("formula parse error at {pos}: {msg}")]
pub struct ParseError {
    pub pos: usize,
    pub msg: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use xlmodel::Cell;

    fn p(s: &str) -> Expr {
        parse(s, Cell { row: 10, col: 3 }).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    #[test]
    fn precedence() {
        // -2^2 = 4 in Excel: negation binds tighter than power.
        assert_eq!(
            p("-2^2"),
            Expr::Binary(BinOp::Pow, Box::new(Expr::Unary(UnOp::Neg, Box::new(Expr::Number(2.0)))), Box::new(Expr::Number(2.0)))
        );
        assert!(matches!(p("1+2*3"), Expr::Binary(BinOp::Add, _, _)));
        assert!(matches!(p("1&2=3"), Expr::Binary(BinOp::Eq, _, _)));
        assert!(matches!(p("2^3^2"), Expr::Binary(BinOp::Pow, l, _) if matches!(*l, Expr::Binary(BinOp::Pow, ..))));
    }

    #[test]
    fn references() {
        // Origin C10: B9 is (-1,-1) relative.
        assert_eq!(
            p("B9"),
            Expr::Ref(Reference { sheet: None, area: Area::Cell { row: Coord::Rel(-1), col: Coord::Rel(-1) } })
        );
        assert_eq!(
            p("'My Sheet'!$A$1:B2"),
            Expr::Ref(Reference {
                sheet: Some("My Sheet".into()),
                area: Area::Range { r1: Coord::Abs(1), c1: Coord::Abs(1), r2: Coord::Rel(-8), c2: Coord::Rel(-1) }
            })
        );
        assert!(matches!(p("A:A"), Expr::Ref(Reference { area: Area::Cols { .. }, .. })));
        assert!(matches!(p("$3:$5"), Expr::Ref(Reference { area: Area::Rows { .. }, .. })));
        assert!(matches!(p("3:5"), Expr::Ref(Reference { area: Area::Rows { .. }, .. })));
        assert!(matches!(p("Sheet1!#REF!"), Expr::RefError));
        assert!(matches!(p("A1#"), Expr::Unary(UnOp::Spill, _)));
        assert!(matches!(p("INDEX(A1:A3,1):INDEX(A1:A3,2)"), Expr::Binary(BinOp::Range, ..)));
    }

    #[test]
    fn functions_and_names() {
        assert_eq!(p("_xlfn.LET(_xlpm.x,1,_xlpm.x+1)"), {
            Expr::Call(
                "LET".into(),
                vec![
                    Expr::Name("X".into()),
                    Expr::Number(1.0),
                    Expr::Binary(BinOp::Add, Box::new(Expr::Name("X".into())), Box::new(Expr::Number(1.0))),
                ],
            )
        });
        assert_eq!(p("IF(A1,,2)"), {
            let Expr::Call(_, a) = p("IF(A1,,2)") else { unreachable!() };
            Expr::Call("IF".into(), vec![a[0].clone(), Expr::Missing, Expr::Number(2.0)])
        });
        assert!(matches!(p("{1,2;3,-4}"), Expr::Array(r) if r.len() == 2 && r[1][1] == Expr::Number(-4.0)));
        assert!(matches!(p("\"a\"\"b\""), Expr::Text(t) if &*t == "a\"b"));
        assert!(matches!(p("LOG10(100)"), Expr::Call(n, _) if &*n == "LOG10"));
        assert!(matches!(p("TRUE"), Expr::Bool(true)));
        assert!(matches!(p("#N/A"), Expr::Error(xlmodel::XlError::NA)));
        assert!(matches!(p("SUM(A1 B1)"), Expr::Call(_, a) if matches!(a[0], Expr::Binary(BinOp::Intersect, ..))));
        assert!(matches!(p("A1 + B1"), Expr::Binary(BinOp::Add, ..)));
        assert!(matches!(p("50%"), Expr::Unary(UnOp::Percent, _)));
        assert!(matches!(p("1.5E+3"), Expr::Number(n) if n == 1500.0));
    }
}
