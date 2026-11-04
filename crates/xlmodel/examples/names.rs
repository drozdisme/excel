//! names WORKBOOK [filter]: list defined names (scope, refers_to).
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let wb = xlmodel::read_workbook(a[1].as_ref(), &xlmodel::ReadOptions { sheets: Some(vec![]) }).unwrap();
    let f = a.get(2).map(|s| s.to_lowercase());
    for n in &wb.names {
        if f.as_ref().is_none_or(|f| n.name.to_lowercase().contains(f)) {
            let scope = n.scope.map(|s| wb.sheets[s].name.clone()).unwrap_or_else(|| "(book)".into());
            println!("{:<34} {:<16} {}", n.name, scope, n.refers_to);
        }
    }
}
