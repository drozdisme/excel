use crate::{
    parse_a1, parse_rect, Cell, CellValue, DefinedName, Formula, FormulaKind, ModelError, Sheet, Workbook, XlError,
};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek};
use std::path::Path;
use std::sync::Arc;

#[derive(Default, Clone)]
pub struct ReadOptions {
    /// Load only these sheets (case-insensitive). `None` loads all.
    pub sheets: Option<Vec<String>>,
}

type Zip = zip::ZipArchive<BufReader<File>>;

pub fn read_workbook(path: &Path, opts: &ReadOptions) -> Result<Workbook, ModelError> {
    let file = BufReader::with_capacity(1 << 20, File::open(path)?);
    let mut zip = zip::ZipArchive::new(file)?;
    let rels = read_rels(&mut zip, "xl/_rels/workbook.xml.rels")?;
    let mut wb = Workbook::default();
    let sheet_parts = read_workbook_xml(&mut zip, &rels, &mut wb)?;
    let strings = match zip.by_name("xl/sharedStrings.xml") {
        Ok(f) => read_shared_strings(BufReader::with_capacity(1 << 20, f))?,
        Err(zip::result::ZipError::FileNotFound) => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    for (i, part) in sheet_parts.iter().enumerate() {
        let wanted = opts
            .sheets
            .as_ref()
            .is_none_or(|list| list.iter().any(|n| n.eq_ignore_ascii_case(&wb.sheets[i].name)));
        if !wanted {
            continue;
        }
        let Some(part) = part else { continue };
        let f = zip.by_name(part)?;
        read_sheet(BufReader::with_capacity(1 << 20, f), part, &strings, &mut wb.sheets[i])?;
    }
    Ok(wb)
}

fn xml_err(part: &str, e: impl std::fmt::Display) -> ModelError {
    ModelError::Xml { part: part.to_string(), msg: e.to_string() }
}

fn attr(e: &BytesStart, key: &str) -> Option<String> {
    e.attributes().flatten().find(|a| a.key.local_name().as_ref() == key).and_then(|a| a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok().map(|v| v.into_owned()))
}

fn read_part<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Result<String, ModelError> {
    let mut s = String::new();
    zip.by_name(name)?.read_to_string(&mut s)?;
    Ok(s)
}

fn read_rels(zip: &mut Zip, part: &str) -> Result<HashMap<String, String>, ModelError> {
    let text = read_part(zip, part)?;
    let mut r = Reader::from_str(&text);
    let mut out = HashMap::new();
    loop {
        match r.read_event().map_err(|e| xml_err(part, e))? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "Relationship" => {
                if let (Some(id), Some(target)) = (attr(&e, "Id"), attr(&e, "Target")) {
                    let t = if let Some(abs) = target.strip_prefix('/') { abs.to_string() } else { format!("xl/{target}") };
                    out.insert(id, normalize_path(&t));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

fn normalize_path(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn read_workbook_xml(zip: &mut Zip, rels: &HashMap<String, String>, wb: &mut Workbook) -> Result<Vec<Option<String>>, ModelError> {
    let part = "xl/workbook.xml";
    let text = read_part(zip, part)?;
    let mut r = Reader::from_str(&text);
    let mut parts = Vec::new();
    let mut pending_name: Option<DefinedName> = None;
    let mut name_text = String::new();
    loop {
        match r.read_event().map_err(|e| xml_err(part, e))? {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                "sheet" => {
                    let name = attr(&e, "name").ok_or_else(|| ModelError::Structure("sheet without name".into()))?;
                    let hidden = attr(&e, "state").is_some_and(|s| s != "visible");
                    let target = attr(&e, "id").and_then(|id| rels.get(&id).cloned());
                    wb.sheets.push(Sheet { name, hidden, ..Default::default() });
                    parts.push(target);
                }
                "workbookPr" => wb.date1904 = attr(&e, "date1904").is_some_and(|v| v == "1" || v == "true"),
                "definedName" => {
                    pending_name = Some(DefinedName {
                        name: attr(&e, "name").unwrap_or_default(),
                        scope: attr(&e, "localSheetId").and_then(|v| v.parse().ok()),
                        refers_to: String::new(),
                        hidden: attr(&e, "hidden").is_some_and(|v| v == "1" || v == "true"),
                    });
                    name_text.clear();
                }
                _ => {}
            },
            Event::Text(t) if pending_name.is_some() => name_text.push_str(&t.xml10_content()),
            Event::GeneralRef(g) if pending_name.is_some() => push_ref(&mut name_text, &g),
            Event::End(e) if e.local_name().as_ref() == "definedName" => {
                if let Some(mut n) = pending_name.take() {
                    n.refers_to = std::mem::take(&mut name_text);
                    wb.names.push(n);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(parts)
}

fn push_ref(buf: &mut String, g: &quick_xml::events::BytesRef) {
    if let Ok(Some(ch)) = g.resolve_char_ref() {
        buf.push(ch);
        return;
    }
    buf.push_str(match &**g {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        _ => "",
    });
}

/// Decode OOXML `_xHHHH_` escapes used for control characters in strings.
fn decode_ooxml(s: String) -> String {
    if !s.contains("_x") {
        return s;
    }
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'_' && i + 6 < b.len() && b[i + 1] == b'x' && b[i + 6] == b'_' && s.is_char_boundary(i + 6)
            && let Ok(code) = u32::from_str_radix(&s[i + 2..i + 6], 16)
                && let Some(ch) = char::from_u32(code) {
                    out.push(ch);
                    i += 7;
                    continue;
                }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn read_shared_strings<R: BufRead>(src: R) -> Result<Vec<Arc<str>>, ModelError> {
    let part = "xl/sharedStrings.xml";
    let mut r = Reader::from_reader(src);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut in_t, mut in_rph) = (false, false);
    loop {
        match r.read_event_into(&mut buf).map_err(|e| xml_err(part, e))? {
            Event::Start(e) => match e.local_name().as_ref() {
                "si" => cur.clear(),
                "t" => in_t = true,
                "rPh" => in_rph = true,
                _ => {}
            },
            Event::Empty(e) if e.local_name().as_ref() == "si" => out.push(Arc::from("")),
            Event::Text(t) if in_t && !in_rph => cur.push_str(&t.xml10_content()),
            Event::GeneralRef(g) if in_t && !in_rph => push_ref(&mut cur, &g),
            Event::End(e) => match e.local_name().as_ref() {
                "t" => in_t = false,
                "rPh" => in_rph = false,
                "si" => out.push(Arc::from(decode_ooxml(std::mem::take(&mut cur)))),
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

#[derive(PartialEq)]
enum Capture {
    None,
    Formula,
    Value,
    Inline,
}

fn read_sheet<R: BufRead>(src: R, part: &str, strings: &[Arc<str>], sheet: &mut Sheet) -> Result<(), ModelError> {
    let mut r = Reader::from_reader(src);
    let mut buf = Vec::with_capacity(4096);
    let mut shared: HashMap<String, (Arc<str>, Cell)> = HashMap::new();
    let (mut row, mut col) = (0u32, 0u32);
    // Current cell state.
    let mut cell = Cell { row: 0, col: 0 };
    let mut ctype = String::new();
    let mut dynamic = false;
    let mut fattrs: Option<(Option<String>, Option<String>, Option<String>)> = None; // (t, ref, si)
    let mut ftext = String::new();
    let mut vtext = String::new();
    let mut cap = Capture::None;
    let mut in_cell = false;
    let mut in_sheet_data = false;

    loop {
        let ev = r.read_event_into(&mut buf).map_err(|e| xml_err(part, e))?;
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(ev, Event::Empty(_));
                match e.local_name().as_ref() {
                    "sheetData" => in_sheet_data = !empty,
                    "row" if in_sheet_data => {
                        row = attr(e, "r").and_then(|v| v.parse().ok()).unwrap_or(row + 1);
                        col = 0;
                    }
                    "c" if in_sheet_data => {
                        cell = match attr(e, "r").and_then(|a| parse_a1(&a)) {
                            Some(c) => c,
                            None => Cell { row, col: col + 1 },
                        };
                        col = cell.col;
                        ctype = attr(e, "t").unwrap_or_default();
                        dynamic = attr(e, "cm").is_some();
                        fattrs = None;
                        ftext.clear();
                        vtext.clear();
                        in_cell = !empty;
                    }
                    "f" if in_cell => {
                        fattrs = Some((attr(e, "t"), attr(e, "ref"), attr(e, "si")));
                        if !empty {
                            cap = Capture::Formula;
                        }
                    }
                    "v" if in_cell && !empty => cap = Capture::Value,
                    "t" if in_cell && !empty => cap = Capture::Inline,
                    _ => {}
                }
            }
            Event::Text(ref t) => match cap {
                Capture::Formula => ftext.push_str(&t.xml10_content()),
                Capture::Value | Capture::Inline => vtext.push_str(&t.xml10_content()),
                Capture::None => {}
            },
            Event::GeneralRef(ref g) => match cap {
                Capture::Formula => push_ref(&mut ftext, g),
                Capture::Value | Capture::Inline => push_ref(&mut vtext, g),
                Capture::None => {}
            },
            Event::End(ref e) => match e.local_name().as_ref() {
                "f" | "v" | "t" => cap = Capture::None,
                "sheetData" => in_sheet_data = false,
                "c" if in_cell => {
                    in_cell = false;
                    finish_cell(sheet, cell, &ctype, dynamic, fattrs.take(), &ftext, &vtext, strings, &mut shared, part)?;
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn finish_cell(
    sheet: &mut Sheet,
    cell: Cell,
    ctype: &str,
    dynamic: bool,
    fattrs: Option<(Option<String>, Option<String>, Option<String>)>,
    ftext: &str,
    vtext: &str,
    strings: &[Arc<str>],
    shared: &mut HashMap<String, (Arc<str>, Cell)>,
    part: &str,
) -> Result<(), ModelError> {
    let value = match ctype {
        "s" => {
            let i: usize = vtext.trim().parse().map_err(|_| xml_err(part, "bad shared string index"))?;
            CellValue::Text(strings.get(i).cloned().ok_or_else(|| xml_err(part, "shared string out of range"))?)
        }
        "inlineStr" | "str" => CellValue::Text(Arc::from(decode_ooxml(vtext.to_string()))),
        "b" => CellValue::Bool(vtext.trim() == "1"),
        "e" => CellValue::Error(XlError::parse(vtext.trim()).unwrap_or(XlError::Value)),
        _ if vtext.is_empty() => CellValue::Empty,
        _ => CellValue::Number(vtext.trim().parse().map_err(|_| xml_err(part, format!("bad number '{vtext}'")))?),
    };
    if value != CellValue::Empty {
        sheet.set(cell, value);
    }
    let Some((ft, fref, si)) = fattrs else { return Ok(()) };
    let formula = match ft.as_deref() {
        Some("shared") => {
            let si = si.ok_or_else(|| xml_err(part, "shared formula without si"))?;
            if !ftext.is_empty() {
                let entry = (Arc::<str>::from(ftext), cell);
                shared.insert(si, entry.clone());
                Formula { text: entry.0, origin: entry.1, kind: FormulaKind::Normal }
            } else {
                let (text, origin) = shared.get(&si).cloned().ok_or_else(|| xml_err(part, format!("shared formula {si} before its master")))?;
                Formula { text, origin, kind: FormulaKind::Normal }
            }
        }
        Some("array") => {
            let range = fref.as_deref().and_then(parse_rect).unwrap_or(crate::Rect::single(cell));
            for r in range.r1..=range.r2 {
                for c in range.c1..=range.c2 {
                    sheet.array_members.insert(Cell { row: r, col: c }, cell);
                }
            }
            Formula { text: Arc::from(ftext), origin: cell, kind: FormulaKind::Array { range, dynamic } }
        }
        // What-if data tables: kept as an unsupported call so only a reachable one fails.
        Some("dataTable") => Formula { text: Arc::from("_BC_DATATABLE()"), origin: cell, kind: FormulaKind::Normal },
        // `<f ca="1"/>` placeholders inside a spill range carry no formula of their own.
        _ if ftext.trim().is_empty() => return Ok(()),
        _ => Formula { text: Arc::from(ftext), origin: cell, kind: FormulaKind::Normal },
    };
    sheet.formulas.insert(cell, formula);
    Ok(())
}
