//! BatchCalc CLI. `frame` runs the Frame STA403 section stage against the workbook's own
//! saved macro outputs (golden) and reports mismatches and timing.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;
use xlcalc::{Lanes, Model, Scalar};
use xlmodel::{Cell, CellValue};

struct FrameJob {
    i: u32,
    location: String,
    set: &'static str,
    k: u32,
    skip: bool,
}

fn uniform(job: &mut xlcalc::Job, sheet: usize, c: Cell) -> Scalar {
    job.cell(sheet, c).expect("eval").as_uniform().cloned().expect("uniform")
}

fn scalar_text(s: &Scalar) -> String {
    match s {
        Scalar::Text(t) => t.to_string(),
        Scalar::Num(n) => xlcalc::scalar::number_to_text(*n),
        Scalar::Empty => String::new(),
        other => format!("{other:?}"),
    }
}

/// (golden sheet, source cells on Section_Analysis) for the section stage.
const SPECS: &[(&str, &str)] = &[
    ("MS", "D59:D70"),
    ("Load_Output", "D77:D80"),
    ("CG_Load_Output", "F77:F80"),
    ("Stress_Output", "F72:F75"),
    ("Gross_Stress_Output", "H72:H75"),
    ("CG_Stress_Output", "J72:J75"),
];

fn same(a: &Scalar, golden: &CellValue) -> bool {
    match (a, golden) {
        (Scalar::Num(x), CellValue::Number(y)) => (x - y).abs() <= 1e-9 + 1e-9 * y.abs(),
        (Scalar::Text(x), CellValue::Text(y)) => x == y,
        (Scalar::Bool(x), CellValue::Bool(y)) => x == y,
        (Scalar::Err(x), CellValue::Error(y)) => x == y,
        (Scalar::Empty, CellValue::Empty) => true,
        _ => false,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 || args[1] != "frame" {
        eprintln!("usage: batchcalc frame WORKBOOK [cases] [threads] [location,...]");
        std::process::exit(2);
    }
    let cases: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
    let threads: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4));
    let only: Option<Vec<String>> = args.get(5).map(|s| s.split(',').map(str::to_string).collect());

    let t0 = Instant::now();
    let wb = xlmodel::read_workbook(args[2].as_ref(), &Default::default()).expect("read workbook");
    let t_read = t0.elapsed().as_secs_f64();
    let model = Model::new(wb).expect("model");
    let t_model = t0.elapsed().as_secs_f64() - t_read;
    let s = |name: &str| model.sheet_index(name).unwrap_or_else(|| panic!("sheet {name}"));
    let (sa, node, loads, lcs_sheet, cfg) = (s("Section_Analysis"), s("Node_Map"), s("Frame_Loads"), s("Loadcases"), s("VBA_Config"));

    // Contract inputs exactly as the VBA macro reads them.
    let mut probe = xlcalc::Job::new(&model, 1);
    let lc_count = match uniform(&mut probe, lcs_sheet, Cell { row: 1, col: 3 }) {
        Scalar::Num(n) => n as usize,
        other => panic!("Loadcases!C1 = {other:?}"),
    };
    let option = match uniform(&mut probe, cfg, Cell { row: 5, col: 3 }) {
        Scalar::Num(n) => n as u32,
        other => panic!("VBA_Config!C5 = {other:?}"),
    };
    let n = if cases > 0 { cases.min(lc_count) } else { lc_count };
    let lc_ids: Vec<Scalar> =
        (0..n).map(|j| Scalar::text(scalar_text(&Scalar::from(model.wb.sheets[loads].value(Cell { row: 2, col: 16 + 3 * j as u32 }))))).collect();
    let mut jobs = Vec::new();
    for i in 1..=100u32 {
        let flag = match Scalar::from(model.wb.sheets[node].value(Cell { row: 55 + i, col: 1 })) {
            Scalar::Num(f) => f as i32,
            _ => match uniform(&mut probe, node, Cell { row: 55 + i, col: 1 }) {
                Scalar::Num(f) => f as i32,
                _ => 0,
            },
        };
        let id = scalar_text(&uniform(&mut probe, node, Cell { row: 55 + i, col: 2 }));
        if id.is_empty() || flag == 4 {
            continue;
        }
        if only.as_ref().is_some_and(|o| !o.contains(&id)) {
            continue;
        }
        for k in 0..2u32 {
            let set = if option == 1 || i <= 50 {
                if k == 0 { "A" } else { "B" }
            } else if k == 0 {
                "B"
            } else {
                "A"
            };
            let skip = (flag == 1 && set == "A") || (flag == 3 && set == "B");
            jobs.push(FrameJob { i, location: id.clone(), set, k, skip });
        }
    }
    drop(probe);
    println!(
        "read {t_read:.2}s, model {t_model:.2}s; {lc_count} load cases (using {n}), option {option}, {} jobs ({} active), {threads} threads",
        jobs.len(),
        jobs.iter().filter(|j| !j.skip).count()
    );

    let sources: Vec<(usize, xlmodel::Rect)> = SPECS.iter().map(|(sheet, src)| (s(sheet), xlmodel::parse_rect(src).unwrap())).collect();
    let frozen: Vec<usize> = ["Joint_Analysis", "Shear_Tie_Analysis", "Geometry_Output"].iter().filter_map(|x| model.sheet_index(x)).collect();
    let cleared: Vec<usize> =
        ["CG_Load_Output", "Load_Output", "CG_Stress_Output", "Stress_Output", "Gross_Stress_Output", "MS"].iter().filter_map(|x| model.sheet_index(x)).collect();

    let next = AtomicUsize::new(0);
    let stats = Mutex::new((0usize, 0usize, Vec::<String>::new(), 0usize, 0f64));
    let t_calc = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..threads.min(jobs.len()).max(1) {
            std::thread::Builder::new()
                .stack_size(1 << 30)
                .spawn_scoped(scope, || loop {
                    let idx = next.fetch_add(1, Ordering::Relaxed);
                    let Some(fj) = jobs.get(idx) else { break };
                    if fj.skip {
                        continue;
                    }
                    let started = Instant::now();
                    let mut job = xlcalc::Job::new(&model, n);
                    for &f in &frozen {
                        job.freeze_sheet(f);
                    }
                    for &c in &cleared {
                        job.clear_sheet(c);
                    }
                    job.set_override(cfg, Cell { row: 4, col: 6 }, Lanes::Uniform(Scalar::text("Y")));
                    job.set_override(sa, Cell { row: 60, col: 11 }, Lanes::Uniform(Scalar::text(fj.location.as_str())));
                    job.set_override(sa, Cell { row: 61, col: 11 }, Lanes::Uniform(Scalar::text(fj.set)));
                    job.set_override(sa, Cell { row: 62, col: 11 }, Lanes::from_vec(lc_ids.clone()));
                    let (mut ok, mut bad, mut msgs) = (0usize, 0usize, Vec::new());
                    for (spec, (out_sheet, rect)) in SPECS.iter().zip(&sources) {
                        for (o, row) in (rect.r1..=rect.r2).enumerate() {
                            for col in rect.c1..=rect.c2 {
                                let lanes = match job.cell(sa, Cell { row, col }) {
                                    Ok(l) => l,
                                    Err(e) => {
                                        if msgs.len() < 3 {
                                            msgs.push(format!("{} {}: {e}", fj.location, fj.set));
                                        }
                                        bad += n;
                                        continue;
                                    }
                                };
                                let golden_sheet = &model.wb.sheets[*out_sheet];
                                for j in 0..n {
                                    let (gr, gc) = if spec.0 == "MS" {
                                        (((fj.i - 1) * 2 + fj.k) * 12 + o as u32 + 5, j as u32 + 5)
                                    } else {
                                        ((fj.i - 1) * 2 + fj.k + 4, 4 + 4 * j as u32 + o as u32)
                                    };
                                    let g = golden_sheet.value(Cell { row: gr, col: gc });
                                    let v = lanes.get(j);
                                    if same(&v, g) {
                                        ok += 1;
                                    } else {
                                        bad += 1;
                                        if msgs.len() < 3 {
                                            msgs.push(format!(
                                                "{} {} LC#{} {}<-{}{}: engine {:?} vs Excel {:?}",
                                                fj.location,
                                                fj.set,
                                                j + 1,
                                                spec.0,
                                                xlmodel::col_name(col),
                                                row,
                                                v,
                                                g
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    let secs = started.elapsed().as_secs_f64();
                    let mut st = stats.lock().unwrap();
                    st.0 += ok;
                    st.1 += bad;
                    st.3 += job.evaluated_cells;
                    st.4 += secs;
                    if st.2.len() < 30 {
                        st.2.extend(msgs);
                    }
                    println!("{:>3} {:<8} {} {:>7.3}s cells={} ok={} bad={}", fj.i, fj.location, fj.set, secs, job.evaluated_cells, ok, bad);
                })
                .unwrap();
        }
    });
    let wall = t_calc.elapsed().as_secs_f64();
    let st = stats.into_inner().unwrap();
    for m in &st.2 {
        println!("  MISMATCH {m}");
    }
    println!("compared {} values: {} equal, {} different; wall {:.2}s (sum of job time {:.1}s)", st.0 + st.1, st.0, st.1, wall, st.4);
}
