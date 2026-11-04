//! Excel binary workbook (.xlsb, BIFF12) reader. Cell formulas are decompiled from their
//! parsed-token form (Rgce) back to A1 text, so the regular formula parser and the
//! shared/array formula machinery are reused unchanged.
use crate::ftab::{FTAB, FTAB_ARGC};
use crate::{a1, col_name, Cell, CellValue, DefinedName, Formula, FormulaKind, ModelError, Rect, Sheet, Workbook, XlError};
use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;

type Zip = zip::ZipArchive<std::io::BufReader<std::fs::File>>;

fn err(part: &str, msg: impl Into<String>) -> ModelError {
    ModelError::Xml { part: part.into(), msg: msg.into() }
}

struct Records<'a> {
    b: &'a [u8],
    i: usize,
}
impl<'a> Iterator for Records<'a> {
    type Item = (u32, &'a [u8]);
    fn next(&mut self) -> Option<Self::Item> {
        let b = self.b;
        if self.i >= b.len() {
            return None;
        }
        let mut t = b[self.i] as u32;
        self.i += 1;
        if t & 0x80 != 0 {
            t = (t & 0x7F) | ((*b.get(self.i)? as u32) << 7);
            self.i += 1;
        }
        let (mut size, mut shift) = (0usize, 0);
        for _ in 0..4 {
            let x = *b.get(self.i)?;
            self.i += 1;
            size |= ((x & 0x7F) as usize) << shift;
            shift += 7;
            if x & 0x80 == 0 {
                break;
            }
        }
        let end = (self.i + size).min(b.len());
        let d = &b[self.i..end];
        self.i = end;
        Some((t, d))
    }
}

/// Little-endian cursor over a record body.
struct Rd<'a> {
    b: &'a [u8],
    i: usize,
}
impl<'a> Rd<'a> {
    fn new(b: &'a [u8]) -> Self {
        Rd { b, i: 0 }
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.i..self.i + n)?;
        self.i += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|s| s[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|s| u16::from_le_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|s| u32::from_le_bytes(s.try_into().unwrap()))
    }
    fn i32(&mut self) -> Option<i32> {
        self.u32().map(|v| v as i32)
    }
    fn f64(&mut self) -> Option<f64> {
        self.take(8).map(|s| f64::from_le_bytes(s.try_into().unwrap()))
    }
    fn wstr(&mut self) -> Option<String> {
        let n = self.u32()?;
        if n == 0xFFFF_FFFF {
            return Some(String::new());
        }
        let s = self.take(n as usize * 2)?;
        Some(String::from_utf16_lossy(&s.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>()))
    }
    fn rest(&self) -> usize {
        self.b.len().saturating_sub(self.i)
    }
}

fn error_code(e: u8) -> XlError {
    match e {
        0x00 => XlError::Null,
        0x07 => XlError::Div0,
        0x0F => XlError::Value,
        0x17 => XlError::Ref,
        0x1D => XlError::Name,
        0x24 => XlError::Num,
        0x2A => XlError::NA,
        0x2B => XlError::GettingData,
        _ => XlError::Value,
    }
}

fn rk(v: u32) -> f64 {
    let n = if v & 2 != 0 { ((v as i32) >> 2) as f64 } else { f64::from_bits(((v & 0xFFFF_FFFC) as u64) << 32) };
    if v & 1 != 0 { n / 100.0 } else { n }
}

/// Workbook-level context needed to decompile formulas.
struct Ctx {
    /// Sheet whose formulas are being decoded (sheet-local names need no prefix there).
    cur: Option<usize>,
    sheets: Vec<String>,
    names: Vec<(String, Option<usize>)>,
    xti: Vec<i32>,
}

impl Ctx {
    fn sheet_prefix(&self, ixti: u16) -> String {
        match self.xti.get(ixti as usize).and_then(|&t| (t >= 0).then(|| self.sheets.get(t as usize)).flatten()) {
            Some(s) => quote_sheet(s) + "!",
            None => "#REF!".into(),
        }
    }
}

fn quote_sheet(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        && !s.chars().next().unwrap().is_ascii_digit()
        && crate::parse_a1(s).is_none();
    if plain { s.to_string() } else { format!("'{}'", s.replace('\'', "''")) }
}

/// Decompile a parsed formula at `at` (row/col 1-based). Returns Ok(None) for a PtgExp
/// (shared/array member) with the anchor row.
enum Decoded {
    Text(String),
    Exp(u32),
}

fn col_ref(c: u16, base: u32, n_form: bool) -> (String, u32) {
    let rel = c & 0x4000 != 0;
    let raw = (c & 0x3FFF) as i32;
    let col = if n_form && rel {
        let off = if raw & 0x2000 != 0 { raw - 0x4000 } else { raw };
        (base as i32 + off).rem_euclid(16384) as u32 + 1
    } else {
        raw as u32 + 1
    };
    (format!("{}{}", if rel { "" } else { "$" }, col_name(col)), col)
}

fn row_ref(r: i32, rel: bool, base: u32, n_form: bool) -> (String, u32) {
    let row = if n_form && rel { (base as i64 + r as i64).rem_euclid(1_048_576) as u32 + 1 } else { r as u32 + 1 };
    (format!("{}{}", if rel { "" } else { "$" }, row), row)
}

fn decompile(rgce: &[u8], rgcb: &[u8], at: Cell, ctx: &Ctx) -> Result<Decoded, String> {
    let mut st: Vec<String> = Vec::new();
    let mut r = Rd::new(rgce);
    let mut extra = Rd::new(rgcb);
    let (base_r, base_c) = (at.row - 1, at.col - 1);
    let pop = |st: &mut Vec<String>| st.pop().ok_or_else(|| "formula stack underflow".to_string());
    while r.rest() > 0 {
        let ptg = r.u8().unwrap();
        let base = if ptg >= 0x20 { (ptg & 0x1F) | 0x20 } else { ptg };
        let bad = || "truncated formula".to_string();
        match base {
            0x01 => {
                let row = r.u32().ok_or_else(bad)?;
                return Ok(Decoded::Exp(row + 1));
            }
            0x02 => return Ok(Decoded::Text("_BC_DATATABLE()".into())),
            0x03..=0x11 => {
                let b = pop(&mut st)?;
                let a = pop(&mut st)?;
                let op = ["+", "-", "*", "/", "^", "&", "<", "<=", "=", ">=", ">", "<>", " ", ",", ":"][(base - 3) as usize];
                st.push(match base {
                    0x0F => format!("{a} {b}"),
                    0x10 => format!("({a},{b})"),
                    0x11 => format!("{a}:{b}"),
                    _ => format!("({a}{op}{b})"),
                });
            }
            0x12 => {
                let a = pop(&mut st)?;
                st.push(format!("+{a}"));
            }
            0x13 => {
                let a = pop(&mut st)?;
                st.push(format!("-{a}"));
            }
            0x14 => {
                let a = pop(&mut st)?;
                st.push(format!("({a})%"));
            }
            0x15 => {
                let a = pop(&mut st)?;
                st.push(format!("({a})"));
            }
            0x16 => st.push(String::new()),
            0x17 => {
                let n = r.u16().ok_or_else(bad)? as usize;
                let s = r.take(n * 2).ok_or_else(bad)?;
                let s = String::from_utf16_lossy(&s.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>());
                st.push(format!("\"{}\"", s.replace('"', "\"\"")));
            }
            0x18 => {
                let eptg = r.u8().ok_or_else(bad)?;
                if eptg == 0x19 {
                    return Ok(Decoded::Text("_BC_UNPARSABLE()".into())); // structured table reference
                }
                return Err(format!("extended ptg 0x18/{eptg:#x}"));
            }
            0x19 => {
                let kind = r.u8().ok_or_else(bad)?;
                let data = r.u16().ok_or_else(bad)?;
                if kind & 0x04 != 0 {
                    r.take((data as usize + 1) * 2).ok_or_else(bad)?;
                }
                if kind & 0x10 != 0 {
                    let a = pop(&mut st)?;
                    st.push(format!("SUM({a})"));
                }
            }
            0x1C => st.push(error_code(r.u8().ok_or_else(bad)?).as_str().to_string()),
            0x1D => st.push(if r.u8().ok_or_else(bad)? != 0 { "TRUE" } else { "FALSE" }.into()),
            0x1E => st.push(r.u16().ok_or_else(bad)?.to_string()),
            0x1F => st.push(fmt_num(r.f64().ok_or_else(bad)?)),
            0x20 => {
                r.take(14).ok_or_else(bad)?;
                st.push(array_const(&mut extra).ok_or("bad array constant")?);
            }
            0x21 | 0x22 => {
                let (argc, tab) = if base == 0x21 {
                    let tab = r.u16().ok_or_else(bad)?;
                    (FTAB_ARGC.get(tab as usize).copied().unwrap_or(0) as usize, tab)
                } else {
                    let argc = r.u8().ok_or_else(bad)? as usize;
                    (argc, r.u16().ok_or_else(bad)? & 0x7FFF)
                };
                if st.len() < argc {
                    return Err("formula stack underflow".into());
                }
                let mut args = st.split_off(st.len() - argc);
                let name = if tab == 255 {
                    let n = args.remove(0);
                    n
                } else {
                    FTAB.get(tab as usize).filter(|s| !s.is_empty()).map(|s| s.to_string()).ok_or_else(|| format!("function #{tab}"))?
                };
                st.push(format!("{name}({})", args.join(",")));
            }
            0x23 => {
                let idx = r.u32().ok_or_else(bad)? as usize;
                let (name, scope) = ctx.names.get(idx.wrapping_sub(1)).cloned().ok_or_else(|| format!("name #{idx}"))?;
                st.push(match scope {
                    Some(s) if Some(s) != ctx.cur => format!("{}!{name}", quote_sheet(&ctx.sheets[s])),
                    _ => name,
                });
            }
            0x24 | 0x2C | 0x3A => {
                let prefix = if base == 0x3A { ctx.sheet_prefix(r.u16().ok_or_else(bad)?) } else { String::new() };
                let row = r.i32().ok_or_else(bad)?;
                let col = r.u16().ok_or_else(bad)?;
                let n = base == 0x2C;
                let (cs, _) = col_ref(col, base_c, n);
                let (rs, _) = row_ref(row, col & 0x8000 != 0, base_r, n);
                st.push(format!("{prefix}{cs}{rs}"));
            }
            0x25 | 0x2D | 0x3B => {
                let prefix = if base == 0x3B { ctx.sheet_prefix(r.u16().ok_or_else(bad)?) } else { String::new() };
                let (r1, r2) = (r.i32().ok_or_else(bad)?, r.i32().ok_or_else(bad)?);
                let (c1, c2) = (r.u16().ok_or_else(bad)?, r.u16().ok_or_else(bad)?);
                let n = base == 0x2D;
                let (c1s, c1n) = col_ref(c1, base_c, n);
                let (c2s, c2n) = col_ref(c2, base_c, n);
                let (r1s, r1n) = row_ref(r1, c1 & 0x8000 != 0, base_r, n);
                let (r2s, r2n) = row_ref(r2, c2 & 0x8000 != 0, base_r, n);
                st.push(if r1n == 1 && r2n == 1_048_576 && !n {
                    format!("{prefix}{c1s}:{c2s}")
                } else if c1n == 1 && c2n == 16_384 && !n {
                    format!("{prefix}{r1s}:{r2s}")
                } else {
                    format!("{prefix}{c1s}{r1s}:{c2s}{r2s}")
                });
            }
            0x26 => {
                r.take(6).ok_or_else(bad)?;
                let count = extra.u32().ok_or("bad mem area")? as usize;
                extra.take(count * 16).ok_or("bad mem area")?;
            }
            0x27 | 0x28 => {
                r.take(6).ok_or_else(bad)?;
            }
            0x29 | 0x2E => {
                r.take(2).ok_or_else(bad)?;
            }
            0x2A => {
                r.take(6).ok_or_else(bad)?;
                st.push("#REF!".into());
            }
            0x2B => {
                r.take(12).ok_or_else(bad)?;
                st.push("#REF!".into());
            }
            0x3C => {
                let p = ctx.sheet_prefix(r.u16().ok_or_else(bad)?);
                r.take(6).ok_or_else(bad)?;
                st.push(format!("{p}#REF!"));
            }
            0x3D => {
                let p = ctx.sheet_prefix(r.u16().ok_or_else(bad)?);
                r.take(12).ok_or_else(bad)?;
                st.push(format!("{p}#REF!"));
            }
            0x39 => {
                r.take(2).ok_or_else(bad)?;
                let idx = r.u32().ok_or_else(bad)? as usize;
                st.push(ctx.names.get(idx.wrapping_sub(1)).map(|n| n.0.clone()).unwrap_or_else(|| "#NAME?".into()));
            }
            other => return Err(format!("unsupported token {other:#x}")),
        }
    }
    if st.len() != 1 {
        return Err(format!("formula left {} values", st.len()));
    }
    Ok(Decoded::Text(st.pop().unwrap()))
}

fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 { format!("{}", v as i64) } else { format!("{v:e}").replace('e', "E") }
}

fn array_const(x: &mut Rd) -> Option<String> {
    let rows = x.u32()? as usize;
    let cols = x.u32()? as usize;
    let mut out = Vec::with_capacity(rows);
    for _ in 0..rows {
        let mut line = Vec::with_capacity(cols);
        for _ in 0..cols {
            let t = x.u8()?;
            line.push(match t {
                0x00 => fmt_num(x.f64()?),
                0x01 => {
                    let n = x.u16()? as usize;
                    let s = x.take(n * 2)?;
                    let s = String::from_utf16_lossy(&s.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>());
                    format!("\"{}\"", s.replace('"', "\"\""))
                }
                0x02 => {
                    let b = x.u8()?;
                    if b != 0 { "TRUE".into() } else { "FALSE".into() }
                }
                0x04 => error_code(x.u8()?).as_str().to_string(),
                _ => return None,
            });
        }
        out.push(line.join(","));
    }
    Some(format!("{{{}}}", out.join(";")))
}

fn read_part(zip: &mut Zip, name: &str) -> Result<Vec<u8>, ModelError> {
    let mut v = Vec::new();
    zip.by_name(name)?.read_to_end(&mut v)?;
    Ok(v)
}

fn parsed_formula(r: &mut Rd) -> Option<(Vec<u8>, Vec<u8>)> {
    let cce = r.u32()? as usize;
    let rgce = r.take(cce)?.to_vec();
    let cb = r.u32().unwrap_or(0) as usize;
    let rgcb = r.take(cb).unwrap_or(&[]).to_vec();
    Some((rgce, rgcb))
}

pub fn read_xlsb(zip: &mut Zip, rels: &HashMap<String, String>, opts: &crate::ReadOptions) -> Result<Workbook, ModelError> {
    let part = "xl/workbook.bin";
    let wbb = read_part(zip, part)?;
    let mut wb = Workbook::default();
    let mut sheet_parts = Vec::new();
    let mut raw_names: Vec<(String, Option<usize>, bool, bool, Vec<u8>, Vec<u8>)> = Vec::new();
    let mut xti = Vec::new();
    for (t, d) in (Records { b: &wbb, i: 0 }) {
        let mut r = Rd::new(d);
        match t {
            153 => wb.date1904 = r.u32().is_some_and(|f| f & 1 != 0),
            156 => {
                let state = r.u32().unwrap_or(0);
                let _tab = r.u32();
                let rid = r.wstr().unwrap_or_default();
                let name = r.wstr().ok_or_else(|| err(part, "sheet name"))?;
                wb.sheets.push(Sheet { name, hidden: state != 0, ..Default::default() });
                sheet_parts.push(rels.get(&rid).cloned());
            }
            39 => {
                let flags = r.u32().unwrap_or(0);
                let _key = r.u8();
                let itab = r.u32().unwrap_or(0xFFFF_FFFF);
                let name = r.wstr().ok_or_else(|| err(part, "name"))?;
                let (rgce, rgcb) = parsed_formula(&mut r).unwrap_or_default();
                raw_names.push((name, (itab != 0xFFFF_FFFF).then_some(itab as usize), flags & 1 != 0, flags & 0x20 != 0, rgce, rgcb));
            }
            362 => {
                let n = r.u32().unwrap_or(0);
                for _ in 0..n {
                    let _sup = r.u32();
                    xti.push(r.i32().unwrap_or(-1));
                    let _last = r.i32();
                }
            }
            _ => {}
        }
    }
    let mut ctx = Ctx {
        cur: None,
        sheets: wb.sheets.iter().map(|s| s.name.clone()).collect(),
        names: raw_names.iter().map(|n| (n.0.clone(), n.1)).collect(),
        xti,
    };
    for (name, scope, hidden, builtin, rgce, rgcb) in &raw_names {
        if name.starts_with("_xlfn.") || rgce.is_empty() {
            continue;
        }
        let refers_to = match decompile(rgce, rgcb, Cell { row: 1, col: 1 }, &ctx) {
            Ok(Decoded::Text(t)) => t,
            _ => "#REF!".into(),
        };
        // Built-in names (Print_Area, _FilterDatabase ...) carry the _xlnm. prefix in xlsx.
        let name = if *builtin && !name.starts_with("_xlnm.") { format!("_xlnm.{name}") } else { name.clone() };
        wb.names.push(DefinedName { name, scope: *scope, refers_to, hidden: *hidden });
    }
    let strings = match read_part(zip, "xl/sharedStrings.bin") {
        Ok(b) => (Records { b: &b, i: 0 })
            .filter(|(t, _)| *t == 19)
            .map(|(_, d)| {
                let mut r = Rd::new(d);
                let _flags = r.u8();
                Arc::<str>::from(r.wstr().unwrap_or_default())
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    for (i, p) in sheet_parts.iter().enumerate() {
        let wanted = opts.sheets.as_ref().is_none_or(|l| l.iter().any(|n| n.eq_ignore_ascii_case(&wb.sheets[i].name)));
        let Some(p) = p else { continue };
        if !wanted {
            continue;
        }
        let data = read_part(zip, p)?;
        ctx.cur = Some(i);
        read_sheet(&data, p, &strings, &ctx, &mut wb.sheets[i])?;
    }
    Ok(wb)
}

fn read_sheet(data: &[u8], part: &str, strings: &[Arc<str>], ctx: &Ctx, sheet: &mut Sheet) -> Result<(), ModelError> {
    let mut row = 0u32;
    let mut meta = false;
    // Cells whose formula is PtgExp (shared or array member) â†’ resolved after all records.
    let mut pending: Vec<(Cell, u32, bool)> = Vec::new();
    let mut last_cell = Cell { row: 0, col: 0 };
    let mut last_dynamic = false;
    let mut shared: Vec<(Rect, Cell, Arc<str>)> = Vec::new();
    let mut arrays: Vec<(Rect, Cell, Arc<str>, bool)> = Vec::new();
    let mut errors = 0usize;
    for (t, d) in (Records { b: data, i: 0 }) {
        let mut r = Rd::new(d);
        match t {
            0 => {
                row = r.u32().unwrap_or(0) + 1;
            }
            49 => meta = true,
            1..=11 => {
                let col = r.u32().ok_or_else(|| err(part, "cell"))? + 1;
                let _style = r.u32();
                let c = Cell { row, col };
                let dynamic = std::mem::take(&mut meta);
                let value = match t {
                    1 => CellValue::Empty,
                    2 => CellValue::Number(rk(r.u32().unwrap_or(0))),
                    3 | 11 => CellValue::Error(error_code(r.u8().unwrap_or(0x0F))),
                    4 | 10 => CellValue::Bool(r.u8().unwrap_or(0) != 0),
                    5 | 9 => CellValue::Number(r.f64().unwrap_or(0.0)),
                    6 | 8 => CellValue::Text(Arc::from(r.wstr().unwrap_or_default())),
                    7 => CellValue::Text(strings.get(r.u32().unwrap_or(0) as usize).cloned().unwrap_or_else(|| Arc::from(""))),
                    _ => CellValue::Empty,
                };
                if value != CellValue::Empty {
                    sheet.set(c, value);
                }
                if (8..=11).contains(&t) {
                    let _flags = r.u16();
                    let Some((rgce, rgcb)) = parsed_formula(&mut r) else { continue };
                    match decompile(&rgce, &rgcb, c, ctx) {
                        Ok(Decoded::Text(text)) => {
                            sheet.formulas.insert(c, Formula { text: Arc::from(text), origin: c, kind: FormulaKind::Normal });
                        }
                        Ok(Decoded::Exp(anchor_row)) => pending.push((c, anchor_row, dynamic)),
                        Err(e) => {
                            errors += 1;
                            if errors <= 3 {
                                eprintln!("xlsb {part} {}: {e}", a1(c));
                            }
                            sheet.formulas.insert(c, Formula { text: Arc::from("_BC_UNPARSABLE()"), origin: c, kind: FormulaKind::Normal });
                        }
                    }
                    last_cell = c;
                    last_dynamic = dynamic;
                }
            }
            426 | 427 => {
                let (r1, r2, c1, c2) = (r.u32().unwrap_or(0) + 1, r.u32().unwrap_or(0) + 1, r.u32().unwrap_or(0) + 1, r.u32().unwrap_or(0) + 1);
                let rect = Rect { r1, c1, r2, c2 };
                if t == 426 {
                    let _flags = r.u8();
                }
                let Some((rgce, rgcb)) = parsed_formula(&mut r) else { continue };
                let anchor = Cell { row: r1, col: c1 };
                let text: Arc<str> = match decompile(&rgce, &rgcb, anchor, ctx) {
                    Ok(Decoded::Text(t)) => Arc::from(t),
                    _ => Arc::from("_BC_UNPARSABLE()"),
                };
                if t == 426 {
                    let dynamic = last_cell == anchor && last_dynamic;
                    arrays.push((rect, anchor, text, dynamic));
                } else {
                    shared.push((rect, anchor, text));
                }
            }
            _ => {}
        }
    }
    for (rect, anchor, text, dynamic) in &arrays {
        sheet.formulas.insert(*anchor, Formula { text: text.clone(), origin: *anchor, kind: FormulaKind::Array { range: *rect, dynamic: *dynamic } });
        for r in rect.r1..=rect.r2 {
            for c in rect.c1..=rect.c2 {
                sheet.array_members.insert(Cell { row: r, col: c }, *anchor);
            }
        }
    }
    for (c, _anchor_row, _) in pending {
        if sheet.array_members.contains_key(&c) {
            continue;
        }
        match shared.iter().find(|(rect, _, _)| rect.contains(c)) {
            Some((_, origin, text)) => {
                sheet.formulas.insert(c, Formula { text: text.clone(), origin: *origin, kind: FormulaKind::Normal });
            }
            None => {
                sheet.formulas.insert(c, Formula { text: Arc::from("_BC_UNPARSABLE()"), origin: c, kind: FormulaKind::Normal });
            }
        }
    }
    Ok(())
}

