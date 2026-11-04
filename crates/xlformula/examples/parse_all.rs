//! Parse every formula and defined name of a workbook; report failures and function usage.
use std::collections::BTreeMap;
use xlformula::{parse, Expr};

fn walk(e: &Expr, f: &mut BTreeMap<String, usize>) {
    match e {
        Expr::Call(n, a) => {
            *f.entry(n.to_string()).or_default() += 1;
            a.iter().for_each(|x| walk(x, f));
        }
        Expr::Unary(_, x) => walk(x, f),
        Expr::Binary(_, a, b) => {
            walk(a, f);
            walk(b, f)
        }
        Expr::Array(rows) => rows.iter().flatten().for_each(|x| walk(x, f)),
        _ => {}
    }
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: parse_all WORKBOOK");
    let wb = xlmodel::read_workbook(path.as_ref(), &Default::default()).expect("read");
    let t = std::time::Instant::now();
    let (mut ok, mut bad) = (0, 0);
    let mut funcs = BTreeMap::new();
    let mut groups = std::collections::HashSet::new();
    for s in &wb.sheets {
        for (cell, f) in &s.formulas {
            groups.insert((s.name.clone(), f.origin, f.text.clone()));
            match parse(&f.text, f.origin) {
                Ok(e) => {
                    ok += 1;
                    walk(&e, &mut funcs);
                }
                Err(e) => {
                    bad += 1;
                    if bad <= 25 {
                        println!("FAIL {}!{}: {e}\n     ={}", s.name, xlmodel::a1(*cell), f.text);
                    }
                }
            }
        }
    }
    let mut nbad = 0;
    for n in &wb.names {
        if let Err(e) = parse(&n.refers_to, xlmodel::Cell { row: 1, col: 1 }) {
            nbad += 1;
            if nbad <= 10 {
                println!("NAME FAIL {}: {e}  ={}", n.name, n.refers_to);
            }
        }
    }
    println!("formulas ok={ok} failed={bad}; distinct groups={}; names failed={nbad}/{}; {:.2}s", groups.len(), wb.names.len(), t.elapsed().as_secs_f64());
    let mut v: Vec<_> = funcs.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    println!("{} functions: {}", v.len(), v.iter().map(|(k, n)| format!("{k}={n}")).collect::<Vec<_>>().join(", "));
}
