//! Print formulas of a sheet range: show WORKBOOK SHEET A1:B10 [max]
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let wb = xlmodel::read_workbook(a[1].as_ref(), &xlmodel::ReadOptions { sheets: Some(vec![a[2].clone()]) }).unwrap();
    let s = wb.sheet(&a[2]).unwrap();
    let r = xlmodel::parse_rect(&a[3]).unwrap();
    let max: usize = a.get(4).map(|x| x.parse().unwrap()).unwrap_or(200);
    let mut n = 0;
    for row in r.r1..=r.r2 {
        for col in r.c1..=r.c2 {
            let c = xlmodel::Cell { row, col };
            if let Some(f) = s.formulas.get(&c) {
                let kind = match f.kind { xlmodel::FormulaKind::Array { range, dynamic } => format!(" [{} {}:{}]", if dynamic {"DA"} else {"CSE"}, xlmodel::a1(xlmodel::Cell{row:range.r1,col:range.c1}), xlmodel::a1(xlmodel::Cell{row:range.r2,col:range.c2})), _ => String::new() };
                let origin = if f.origin != c { format!(" (shared from {})", xlmodel::a1(f.origin)) } else { String::new() };
                println!("{}{}{} = {:?}\n    ={}", xlmodel::a1(c), kind, origin, s.value(c), f.text);
                n += 1;
            } else if s.value(c) != &xlmodel::CellValue::Empty && a.get(5).is_some() {
                println!("{} : {:?}", xlmodel::a1(c), s.value(c));
            }
            if n >= max { return; }
        }
    }
}
