use crate::ast::*;
use crate::lexer::{lex, Tok, Token};
use crate::ParseError;
use std::sync::Arc;
use xlmodel::{Cell, XlError};

/// Parse formula text (with or without leading '=') written at `origin`.
/// Relative references are stored as offsets from `origin`.
pub fn parse(text: &str, origin: Cell) -> Result<Expr, ParseError> {
    let text = text.strip_prefix('=').unwrap_or(text);
    let toks = lex(text)?;
    let mut p = Parser { t: toks, i: 0, origin };
    let e = p.expr(0)?;
    if p.peek() != &Tok::Eof {
        return Err(p.err("unexpected trailing input"));
    }
    Ok(e)
}

struct Parser {
    t: Vec<Token>,
    i: usize,
    origin: Cell,
}

/// Binary operator precedence (higher binds tighter).
fn infix(tok: &Tok) -> Option<(u8, BinOp)> {
    Some(match tok {
        Tok::Op("=") => (1, BinOp::Eq),
        Tok::Op("<>") => (1, BinOp::Ne),
        Tok::Op("<") => (1, BinOp::Lt),
        Tok::Op("<=") => (1, BinOp::Le),
        Tok::Op(">") => (1, BinOp::Gt),
        Tok::Op(">=") => (1, BinOp::Ge),
        Tok::Op("&") => (2, BinOp::Concat),
        Tok::Op("+") => (3, BinOp::Add),
        Tok::Op("-") => (3, BinOp::Sub),
        Tok::Op("*") => (4, BinOp::Mul),
        Tok::Op("/") => (4, BinOp::Div),
        Tok::Op("^") => (5, BinOp::Pow),
        Tok::Colon => (9, BinOp::Range),
        _ => return None,
    })
}
const PREC_PERCENT: u8 = 6;
const PREC_UNARY: u8 = 7;
const PREC_INTERSECT: u8 = 8;

fn starts_primary(t: &Tok) -> bool {
    matches!(t, Tok::Word(_) | Tok::Sheet(_) | Tok::External(_) | Tok::LParen)
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.t[self.i].tok
    }
    fn tok(&self) -> &Token {
        &self.t[self.i]
    }
    fn next(&mut self) -> Tok {
        let t = self.t[self.i].tok.clone();
        if self.i + 1 < self.t.len() {
            self.i += 1;
        }
        t
    }
    fn err(&self, msg: &str) -> ParseError {
        ParseError { pos: self.t[self.i].pos, msg: msg.to_string() }
    }
    fn expect(&mut self, t: Tok, what: &str) -> Result<(), ParseError> {
        if *self.peek() == t {
            self.next();
            Ok(())
        } else {
            Err(self.err(&format!("expected {what}")))
        }
    }

    fn expr(&mut self, min: u8) -> Result<Expr, ParseError> {
        let mut lhs = self.prefix()?;
        loop {
            let t = self.tok().clone();
            if t.tok == Tok::Op("%") {
                if PREC_PERCENT < min {
                    break;
                }
                self.next();
                lhs = Expr::Unary(UnOp::Percent, Box::new(lhs));
                continue;
            }
            if t.tok == Tok::Hash {
                self.next();
                lhs = Expr::Unary(UnOp::Spill, Box::new(lhs));
                continue;
            }
            if let Some((prec, op)) = infix(&t.tok) {
                if prec < min {
                    break;
                }
                self.next();
                // '^' is left-associative in Excel.
                let rhs = self.expr(prec + 1)?;
                lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
                continue;
            }
            if t.space_before && starts_primary(&t.tok) && PREC_INTERSECT >= min && is_reference_like(&lhs) {
                let rhs = self.expr(PREC_INTERSECT + 1)?;
                lhs = Expr::Binary(BinOp::Intersect, Box::new(lhs), Box::new(rhs));
                continue;
            }
            break;
        }
        Ok(lhs)
    }

    fn prefix(&mut self) -> Result<Expr, ParseError> {
        match self.peek().clone() {
            Tok::Op("-") => {
                self.next();
                Ok(Expr::Unary(UnOp::Neg, Box::new(self.expr(PREC_UNARY)?)))
            }
            Tok::Op("+") => {
                self.next();
                Ok(Expr::Unary(UnOp::Plus, Box::new(self.expr(PREC_UNARY)?)))
            }
            Tok::At => {
                self.next();
                Ok(Expr::Unary(UnOp::Implicit, Box::new(self.expr(PREC_INTERSECT + 1)?)))
            }
            _ => self.primary(),
        }
    }

    fn primary(&mut self) -> Result<Expr, ParseError> {
        match self.next() {
            Tok::Number(n) => {
                // Row range like 3:5 (digits only, no spaces around ':').
                if *self.peek() == Tok::Colon && n.fract() == 0.0
                    && let Some(Tok::Number(m)) = self.t.get(self.i + 1).map(|t| t.tok.clone())
                        && !self.t[self.i].space_before && !self.t[self.i + 1].space_before && m.fract() == 0.0 {
                            self.next();
                            self.next();
                            let (r1, r2) = (self.row_coord(n as u32, false), self.row_coord(m as u32, false));
                            return Ok(Expr::Ref(Reference { sheet: None, area: Area::Rows { r1, r2 } }));
                        }
                Ok(Expr::Number(n))
            }
            Tok::Text(s) => Ok(Expr::Text(Arc::from(s))),
            Tok::Error(e) => Ok(Expr::Error(XlError::parse(&e.to_uppercase()).unwrap_or(XlError::Value))),
            Tok::LParen => {
                let e = self.expr(0)?;
                if *self.peek() == Tok::Comma {
                    return Err(self.err("reference union (a,b) is not supported"));
                }
                self.expect(Tok::RParen, "')'")?;
                Ok(e)
            }
            Tok::LBrace => self.array(),
            Tok::External(x) => Err(self.err(&format!("external workbook reference {x} is not supported"))),
            Tok::Sheet(name) => {
                // Optional 3D range Sheet1:Sheet3!A1 is lexed as Word(Sheet1) Colon Sheet(Sheet3); not supported here.
                match self.next() {
                    Tok::Word(w) => self.word_ref(Some(Arc::from(name)), w),
                    Tok::Number(n) => {
                        // Sheet!3:5 row range.
                        let r1 = self.row_coord(n as u32, false);
                        self.expect(Tok::Colon, "':'")?;
                        match self.next() {
                            Tok::Number(m) => Ok(Expr::Ref(Reference {
                                sheet: Some(Arc::from(name)),
                                area: Area::Rows { r1, r2: self.row_coord(m as u32, false) },
                            })),
                            _ => Err(self.err("bad row range")),
                        }
                    }
                    Tok::Error(e) if e.eq_ignore_ascii_case("#REF!") => Ok(Expr::RefError),
                    _ => Err(self.err("expected reference after sheet name")),
                }
            }
            Tok::Word(w) => self.word_ref(None, w),
            t => Err(self.err(&format!("unexpected token {t:?}"))),
        }
    }

    fn array(&mut self) -> Result<Expr, ParseError> {
        let mut rows = vec![Vec::new()];
        loop {
            let neg = if *self.peek() == Tok::Op("-") {
                self.next();
                true
            } else {
                false
            };
            let v = match self.next() {
                Tok::Number(n) => Expr::Number(if neg { -n } else { n }),
                Tok::Text(s) if !neg => Expr::Text(Arc::from(s)),
                Tok::Error(e) if !neg => Expr::Error(XlError::parse(&e.to_uppercase()).unwrap_or(XlError::Value)),
                Tok::Word(w) if !neg && w.eq_ignore_ascii_case("TRUE") => Expr::Bool(true),
                Tok::Word(w) if !neg && w.eq_ignore_ascii_case("FALSE") => Expr::Bool(false),
                _ => return Err(self.err("bad array constant")),
            };
            rows.last_mut().unwrap().push(v);
            match self.next() {
                Tok::Comma => {}
                Tok::Semicolon => rows.push(Vec::new()),
                Tok::RBrace => break,
                _ => return Err(self.err("expected ',', ';' or '}' in array")),
            }
        }
        if rows.iter().any(|r| r.len() != rows[0].len()) {
            return Err(self.err("ragged array constant"));
        }
        Ok(Expr::Array(rows))
    }

    /// A word: function call, TRUE/FALSE, cell/column/row reference or a name.
    fn word_ref(&mut self, sheet: Option<Arc<str>>, w: String) -> Result<Expr, ParseError> {
        if sheet.is_none() && *self.peek() == Tok::LParen {
            self.next();
            let name = normalize_fn(&w);
            let mut args = Vec::new();
            if *self.peek() == Tok::RParen {
                self.next();
                return Ok(Expr::Call(name, args));
            }
            loop {
                if matches!(self.peek(), Tok::Comma | Tok::RParen) {
                    args.push(Expr::Missing);
                } else {
                    args.push(self.expr(0)?);
                }
                match self.next() {
                    Tok::Comma => continue,
                    Tok::RParen => break,
                    _ => return Err(self.err("expected ',' or ')' in argument list")),
                }
            }
            return Ok(Expr::Call(name, args));
        }
        if sheet.is_none() && w.eq_ignore_ascii_case("TRUE") {
            return Ok(Expr::Bool(true));
        }
        if sheet.is_none() && w.eq_ignore_ascii_case("FALSE") {
            return Ok(Expr::Bool(false));
        }
        // Cell reference, possibly the start of a range A1:B2.
        if let Some((row, col)) = self.cell_coords(&w) {
            if *self.peek() == Tok::Colon && !self.tok().space_before
                && let Some(Tok::Word(w2)) = self.t.get(self.i + 1).map(|t| t.tok.clone())
                    && let Some((r2, c2)) = self.cell_coords(&w2) {
                        self.next();
                        self.next();
                        return Ok(Expr::Ref(Reference { sheet, area: Area::Range { r1: row, c1: col, r2, c2 } }));
                    }
            return Ok(Expr::Ref(Reference { sheet, area: Area::Cell { row, col } }));
        }
        // Column range A:C.
        if let Some(c1) = self.col_only(&w)
            && *self.peek() == Tok::Colon
                && let Some(Tok::Word(w2)) = self.t.get(self.i + 1).map(|t| t.tok.clone())
                    && let Some(c2) = self.col_only(&w2) {
                        self.next();
                        self.next();
                        return Ok(Expr::Ref(Reference { sheet, area: Area::Cols { c1, c2 } }));
                    }
        // Row range with '$': $3:$5.
        if let Some(r1) = self.row_only(&w)
            && *self.peek() == Tok::Colon {
                let next = self.t.get(self.i + 1).map(|t| t.tok.clone());
                let r2 = match next {
                    Some(Tok::Word(w2)) => self.row_only(&w2),
                    Some(Tok::Number(m)) if m.fract() == 0.0 => Some(self.row_coord(m as u32, false)),
                    _ => None,
                };
                if let Some(r2) = r2 {
                    self.next();
                    self.next();
                    return Ok(Expr::Ref(Reference { sheet, area: Area::Rows { r1, r2 } }));
                }
            }
        if let Some(s) = sheet {
            // Sheet-scoped defined name: keep as name qualified with the sheet.
            return Ok(Expr::Name(Arc::from(format!("{}!{}", s, normalize_name(&w)))));
        }
        Ok(Expr::Name(Arc::from(normalize_name(&w))))
    }

    fn row_coord(&self, row: u32, abs: bool) -> Coord {
        if abs { Coord::Abs(row) } else { Coord::Rel(row as i32 - self.origin.row as i32) }
    }
    fn col_coord(&self, col: u32, abs: bool) -> Coord {
        if abs { Coord::Abs(col) } else { Coord::Rel(col as i32 - self.origin.col as i32) }
    }

    fn cell_coords(&self, w: &str) -> Option<(Coord, Coord)> {
        let b = w.as_bytes();
        let mut i = 0;
        let cabs = b.first() == Some(&b'$');
        if cabs {
            i += 1;
        }
        let s = i;
        let mut col = 0u32;
        while i < b.len() && b[i].is_ascii_alphabetic() {
            col = col * 26 + (b[i].to_ascii_uppercase() - b'A' + 1) as u32;
            i += 1;
        }
        if i == s || i - s > 3 || col > MAX_COL {
            return None;
        }
        let rabs = b.get(i) == Some(&b'$');
        if rabs {
            i += 1;
        }
        let digits = &w[i..];
        if digits.is_empty() || !digits.bytes().all(|d| d.is_ascii_digit()) || digits.len() > 7 {
            return None;
        }
        let row: u32 = digits.parse().ok()?;
        if row == 0 || row > MAX_ROW {
            return None;
        }
        Some((self.row_coord(row, rabs), self.col_coord(col, cabs)))
    }

    fn col_only(&self, w: &str) -> Option<Coord> {
        let (abs, letters) = match w.strip_prefix('$') {
            Some(r) => (true, r),
            None => (false, w),
        };
        if letters.is_empty() || letters.len() > 3 || !letters.bytes().all(|b| b.is_ascii_alphabetic()) {
            return None;
        }
        let col = letters.bytes().fold(0u32, |a, b| a * 26 + (b.to_ascii_uppercase() - b'A' + 1) as u32);
        (col <= MAX_COL).then(|| self.col_coord(col, abs))
    }

    fn row_only(&self, w: &str) -> Option<Coord> {
        let (abs, digits) = match w.strip_prefix('$') {
            Some(r) => (true, r),
            None => (false, w),
        };
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let row: u32 = digits.parse().ok()?;
        (1..=MAX_ROW).contains(&row).then(|| self.row_coord(row, abs))
    }
}

fn is_reference_like(e: &Expr) -> bool {
    matches!(e, Expr::Ref(_) | Expr::Name(_) | Expr::Binary(BinOp::Range | BinOp::Intersect, ..))
        || matches!(e, Expr::Call(n, _) if matches!(&**n, "INDEX" | "OFFSET" | "INDIRECT"))
}

pub fn normalize_fn(w: &str) -> Arc<str> {
    let mut u = w.to_ascii_uppercase();
    for p in ["_XLFN._XLWS.", "_XLFN.", "_XLWS.", "_XLUDF."] {
        if let Some(r) = u.strip_prefix(p) {
            u = r.to_string();
        }
    }
    Arc::from(u)
}

pub fn normalize_name(w: &str) -> String {
    let u = w.to_ascii_uppercase();
    u.strip_prefix("_XLPM.").map(str::to_string).unwrap_or(u)
}
