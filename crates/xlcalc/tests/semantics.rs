//! Differential semantics test: every formula in tests/semantics_excel.tsv was evaluated by
//! real Excel (tools/Probe-Formulas.ps1, dynamic-array semantics, same input cells); the
//! engine must produce the same spill shape, types and values.
use std::sync::Arc;
use xlcalc::{Job, Lanes, Model, Scalar};
use xlmodel::{Cell, CellValue, Formula, FormulaKind, Rect, Sheet, Workbook, XlError};

fn setup_sheet() -> Sheet {
    let mut s = Sheet { name: "Sheet1".into(), ..Default::default() };
    for (a, v) in [("A1", "1"), ("A2", "2"), ("A3", "3"), ("A4", "4"), ("A5", "5"), ("A6", "x"), ("B1", "a"), ("B2", "b"), ("B3", "c"), ("B4", "bb"), ("B5", "a")] {
        let c = xlmodel::parse_a1(a).unwrap();
        s.set(c, match v.parse::<f64>() {
            Ok(n) => CellValue::Number(n),
            Err(_) => CellValue::Text(Arc::from(v)),
        });
    }
    s
}

fn expected(tok: &str) -> Scalar {
    let (t, v) = tok.split_once('=').unwrap();
    match t {
        "Double" => Scalar::Num(v.parse().unwrap()),
        "String" => Scalar::text(v),
        "Boolean" => Scalar::Bool(v == "True"),
        "empty" => Scalar::Empty,
        e => Scalar::Err(XlError::parse(e.trim_start_matches("err:")).unwrap_or_else(|| panic!("error token {e}"))),
    }
}

fn same(a: &Scalar, b: &Scalar) -> bool {
    match (a, b) {
        (Scalar::Num(x), Scalar::Num(y)) => (x - y).abs() <= 1e-12 * y.abs().max(1.0),
        // A spilled empty cell reads as 0 in Excel's grid; the engine materialises it as 0 too.
        _ => a == b,
    }
}

#[test]
fn matches_real_excel() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/semantics_excel.tsv");
    let text = std::fs::read_to_string(path).expect("semantics_excel.tsv");
    let lines: Vec<&str> = text.trim_start_matches('\u{feff}').lines().filter(|l| !l.trim().is_empty()).collect();
    let mut sheet = setup_sheet();
    let mut cases = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        let (formula, shape, vals) = (parts[0], parts[1], parts[2]);
        let cell = Cell { row: 100 + 12 * i as u32, col: 8 };
        let (r, c) = shape.split_once('x').unwrap();
        let (r, c): (u32, u32) = (r.parse().unwrap(), c.parse().unwrap());
        let range = Rect { r1: cell.row, c1: cell.col, r2: cell.row + r - 1, c2: cell.col + c - 1 };
        let text = formula.trim_start_matches('=');
        sheet.formulas.insert(cell, Formula { text: Arc::from(text), origin: cell, kind: FormulaKind::Array { range: Rect::single(cell), dynamic: true } });
        sheet.array_members.insert(cell, cell);
        cases.push((formula.to_string(), cell, range, vals.split(" | ").map(expected).collect::<Vec<_>>()));
    }
    let wb = Workbook { sheets: vec![sheet], ..Default::default() };
    let model = Model::new(wb).unwrap();
    let mut failures = Vec::new();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn_scoped_compat(|| {
            let mut job = Job::new(&model, 1);
            for (f, cell, range, want) in &cases {
                let mut got = Vec::new();
                for row in range.r1..=range.r2 {
                    for col in range.c1..=range.c2 {
                        got.push(job.cell(0, Cell { row, col }).map(|l: Lanes| l.get(0)).unwrap_or_else(|e| Scalar::text(format!("ENGINE ERROR {e}"))));
                    }
                }
                // The spill must not extend beyond Excel's.
                let beyond = job.cell(0, Cell { row: range.r2 + 1, col: cell.col }).map(|l| l.get(0)).unwrap_or(Scalar::Empty);
                let ok = got.len() == want.len() && got.iter().zip(want).all(|(a, b)| same(a, b)) && beyond == Scalar::Empty;
                if !ok {
                    failures.push(format!("{f}\n   Excel  {want:?}\n   engine {got:?}{}", if beyond != Scalar::Empty { " (spills further)" } else { "" }));
                }
            }
        });
    assert!(failures.is_empty(), "{} of {} formulas differ from Excel:\n{}", failures.len(), cases.len(), failures.join("\n"));
}

trait SpawnCompat {
    fn spawn_scoped_compat<F: FnOnce() + Send>(self, f: F);
}
impl SpawnCompat for std::thread::Builder {
    fn spawn_scoped_compat<F: FnOnce() + Send>(self, f: F) {
        std::thread::scope(|s| {
            self.spawn_scoped(s, f).unwrap().join().unwrap();
        });
    }
}
