use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("usage: stats WORKBOOK");
    let t = Instant::now();
    let wb = xlmodel::read_workbook(path.as_ref(), &Default::default()).expect("read");
    println!("read {:.2}s, {} sheets, {} formulas, {} names", t.elapsed().as_secs_f64(), wb.sheets.len(), wb.formula_count(), wb.names.len());
    for s in &wb.sheets {
        let cells: usize = s.rows.iter().map(|r| r.iter().filter(|v| **v != xlmodel::CellValue::Empty).count()).sum();
        let arrays = s.formulas.values().filter(|f| matches!(f.kind, xlmodel::FormulaKind::Array { .. })).count();
        println!("{:<28} rows={:>7} cols={:>6} values={:>9} formulas={:>7} arrays={:>5}", s.name, s.max_row(), s.max_col(), cells, s.formulas.len(), arrays);
    }
}
