//! Render an AST back to A1 formula text as seen from a given cell (diagnostics).
use crate::ast::*;
use xlmodel::Cell;

fn coord(c: Coord, base: u32, col: bool) -> String {
    match c {
        Coord::Abs(v) => format!("${}", if col { xlmodel::col_name(v) } else { v.to_string() }),
        Coord::Rel(d) => {
            let v = base as i64 + d as i64;
            if v < 1 {
                return "#REF!".into();
            }
            if col { xlmodel::col_name(v as u32) } else { v.to_string() }
        }
    }
}

fn reference(r: &Reference, at: Cell) -> String {
    let s = match &r.sheet {
        Some(s) if s.chars().all(|c| c.is_alphanumeric() || c == '_') => format!("{s}!"),
        Some(s) => format!("'{}'!", s.replace('\'', "''")),
        None => String::new(),
    };
    let body = match &r.area {
        Area::Cell { row, col } => format!("{}{}", coord(*col, at.col, true), coord(*row, at.row, false)),
        Area::Range { r1, c1, r2, c2 } => {
            format!("{}{}:{}{}", coord(*c1, at.col, true), coord(*r1, at.row, false), coord(*c2, at.col, true), coord(*r2, at.row, false))
        }
        Area::Cols { c1, c2 } => format!("{}:{}", coord(*c1, at.col, true), coord(*c2, at.col, true)),
        Area::Rows { r1, r2 } => format!("{}:{}", coord(*r1, at.row, false), coord(*r2, at.row, false)),
    };
    s + &body
}

pub fn to_a1(e: &Expr, at: Cell) -> String {
    match e {
        Expr::Number(n) => format!("{n}"),
        Expr::Text(t) => format!("\"{}\"", t.replace('"', "\"\"")),
        Expr::Bool(b) => if *b { "TRUE" } else { "FALSE" }.into(),
        Expr::Error(x) => x.as_str().into(),
        Expr::Missing => String::new(),
        Expr::Array(rows) => format!("{{{}}}", rows.iter().map(|r| r.iter().map(|x| to_a1(x, at)).collect::<Vec<_>>().join(",")).collect::<Vec<_>>().join(";")),
        Expr::Ref(r) => reference(r, at),
        Expr::RefError => "#REF!".into(),
        Expr::Name(n) => n.to_string(),
        Expr::Call(n, a) => format!("{n}({})", a.iter().map(|x| to_a1(x, at)).collect::<Vec<_>>().join(",")),
        Expr::Unary(op, x) => match op {
            UnOp::Neg => format!("-{}", to_a1(x, at)),
            UnOp::Plus => format!("+{}", to_a1(x, at)),
            UnOp::Percent => format!("{}%", to_a1(x, at)),
            UnOp::Implicit => format!("@{}", to_a1(x, at)),
            UnOp::Spill => format!("{}#", to_a1(x, at)),
        },
        Expr::Binary(op, a, b) => {
            let o = match op {
                BinOp::Add => "+",
                BinOp::Sub => "-",
                BinOp::Mul => "*",
                BinOp::Div => "/",
                BinOp::Pow => "^",
                BinOp::Concat => "&",
                BinOp::Eq => "=",
                BinOp::Ne => "<>",
                BinOp::Lt => "<",
                BinOp::Le => "<=",
                BinOp::Gt => ">",
                BinOp::Ge => ">=",
                BinOp::Range => ":",
                BinOp::Intersect => " ",
            };
            format!("({}{}{})", to_a1(a, at), o, to_a1(b, at))
        }
    }
}

/// Render in R1C1 notation (relative offsets are relative to the formula's own cell, i.e.
/// the origin it was parsed at). Used to hand formulas and names to Excel's macro language.
pub fn to_r1c1(e: &Expr) -> String {
    fn rc(c: Coord, letter: char) -> String {
        match c {
            Coord::Abs(v) => format!("{letter}{v}"),
            Coord::Rel(0) => letter.to_string(),
            Coord::Rel(d) => format!("{letter}[{d}]"),
        }
    }
    fn reference(r: &Reference) -> String {
        let s = match &r.sheet {
            Some(s) if s.chars().all(|c| c.is_alphanumeric() || c == '_') => format!("{s}!"),
            Some(s) => format!("'{}'!", s.replace('\'', "''")),
            None => String::new(),
        };
        s + &match &r.area {
            Area::Cell { row, col } => format!("{}{}", rc(*row, 'R'), rc(*col, 'C')),
            Area::Range { r1, c1, r2, c2 } => format!("{}{}:{}{}", rc(*r1, 'R'), rc(*c1, 'C'), rc(*r2, 'R'), rc(*c2, 'C')),
            Area::Cols { c1, c2 } => format!("{}:{}", rc(*c1, 'C'), rc(*c2, 'C')),
            Area::Rows { r1, r2 } => format!("{}:{}", rc(*r1, 'R'), rc(*r2, 'R')),
        }
    }
    fn go(e: &Expr) -> String {
        match e {
            Expr::Ref(r) => reference(r),
            Expr::Call(n, a) => format!("{n}({})", a.iter().map(go).collect::<Vec<_>>().join(",")),
            Expr::Unary(op, x) => match op {
                UnOp::Neg => format!("-{}", go(x)),
                UnOp::Plus => format!("+{}", go(x)),
                UnOp::Percent => format!("{}%", go(x)),
                UnOp::Implicit => format!("@{}", go(x)),
                UnOp::Spill => format!("{}#", go(x)),
            },
            Expr::Binary(op, a, b) => {
                let o = match op {
                    BinOp::Add => "+",
                    BinOp::Sub => "-",
                    BinOp::Mul => "*",
                    BinOp::Div => "/",
                    BinOp::Pow => "^",
                    BinOp::Concat => "&",
                    BinOp::Eq => "=",
                    BinOp::Ne => "<>",
                    BinOp::Lt => "<",
                    BinOp::Le => "<=",
                    BinOp::Gt => ">",
                    BinOp::Ge => ">=",
                    BinOp::Range => ":",
                    BinOp::Intersect => " ",
                };
                format!("({}{}{})", go(a), o, go(b))
            }
            Expr::Array(rows) => format!("{{{}}}", rows.iter().map(|r| r.iter().map(go).collect::<Vec<_>>().join(",")).collect::<Vec<_>>().join(";")),
            other => to_a1(other, xlmodel::Cell { row: 1, col: 1 }),
        }
    }
    go(e)
}

/// All references in an expression (for dependency diagnostics).
pub fn references(e: &Expr, out: &mut Vec<Reference>) {
    match e {
        Expr::Ref(r) => out.push(r.clone()),
        Expr::Call(_, a) => a.iter().for_each(|x| references(x, out)),
        Expr::Unary(_, x) => references(x, out),
        Expr::Binary(_, a, b) => {
            references(a, out);
            references(b, out)
        }
        Expr::Array(rows) => rows.iter().flatten().for_each(|x| references(x, out)),
        _ => {}
    }
}
