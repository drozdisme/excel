//! Executes contract stages: enumerates jobs, runs them on worker threads over all cases at
//! once, and collects the cells the original macro would have written.
use crate::contract::{Contract, Header, Scope, Stage};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use xlcalc::eval::{Overlay, SheetPatch};
use xlcalc::{Job, Lanes, Model, Scalar, Value};
use xlformula::Expr;
use xlmodel::{Cell, CellValue, Rect};

#[derive(Debug, Clone)]
pub struct Progress {
    pub stage: String,
    pub stage_index: usize,
    pub stage_count: usize,
    pub done: usize,
    pub total: usize,
}

pub struct RunOptions {
    pub threads: usize,
    pub case_limit: Option<usize>,
    pub cancel: Arc<AtomicBool>,
    pub progress: Option<Arc<dyn Fn(&Progress) + Send + Sync>>,
}

impl Default for RunOptions {
    fn default() -> Self {
        RunOptions {
            threads: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4),
            case_limit: None,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct StageReport {
    pub name: String,
    pub jobs: usize,
    pub active_jobs: usize,
    pub cases: usize,
    pub seconds: f64,
    pub cells_written: usize,
    pub excel_errors: usize,
}

pub struct RunResult {
    /// Final content of every sheet the stages cleared or wrote.
    pub patches: Overlay,
    pub reports: Vec<StageReport>,
    /// Cells the macro leaves behind (driver values, RUNNING flags), to write after outputs.
    pub final_state: Vec<(usize, Cell, Scalar)>,
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Contract(String),
    #[error("{0}")]
    Eval(String),
}

type Res<T> = Result<T, RunError>;

fn parse(text: &str) -> Res<Arc<Expr>> {
    xlformula::parse(text, Cell { row: 1, col: 1 }).map(Arc::new).map_err(|e| RunError::Contract(format!("'{text}': {e}")))
}

/// Resolve "Sheet!A1" / "'Sheet x'!A1:B2" to (sheet index, rect).
pub fn resolve_range(model: &Model, s: &str) -> Res<(usize, Rect)> {
    let (sheet, addr) = s.rsplit_once('!').ok_or_else(|| RunError::Contract(format!("'{s}' needs a sheet")))?;
    let sheet = sheet.trim_matches('\'').replace("''", "'");
    let idx = model.sheet_index(&sheet).ok_or_else(|| RunError::Contract(format!("no sheet '{sheet}'")))?;
    let rect = xlmodel::parse_rect(&addr.replace('$', "")).ok_or_else(|| RunError::Contract(format!("bad range '{s}'")))?;
    Ok((idx, rect))
}

fn sheet_idx(model: &Model, name: &str) -> Res<usize> {
    model.sheet_index(name).ok_or_else(|| RunError::Contract(format!("no sheet '{name}'")))
}

/// How a value lands in a cell when written by VBA `Range.Value2 = array`.
fn to_cell(s: &Scalar) -> CellValue {
    match s {
        Scalar::Empty => CellValue::Empty,
        Scalar::Num(n) => CellValue::Number(*n),
        Scalar::Text(t) if t.is_empty() => CellValue::Empty,
        Scalar::Text(t) => CellValue::Text(t.clone()),
        Scalar::Bool(b) => CellValue::Bool(*b),
        Scalar::Err(e) => CellValue::Error(*e),
    }
}

fn place(base: (i64, i64), step: [i32; 2], k: usize, case_step: Option<[i32; 2]>, j: usize) -> Option<Cell> {
    let (cs_r, cs_c) = case_step.map(|c| (c[0] as i64, c[1] as i64)).unwrap_or((0, 0));
    let r = base.0 + step[0] as i64 * k as i64 + cs_r * j as i64;
    let c = base.1 + step[1] as i64 * k as i64 + cs_c * j as i64;
    ((1..=1_048_576).contains(&r) && (1..=16_384).contains(&c)).then_some(Cell { row: r as u32, col: c as u32 })
}

struct Compiled {
    exprs: HashMap<String, Arc<Expr>>,
}

impl Compiled {
    fn get(&mut self, text: &str) -> Res<Arc<Expr>> {
        if let Some(e) = self.exprs.get(text) {
            return Ok(e.clone());
        }
        let e = parse(text)?;
        self.exprs.insert(text.to_string(), e.clone());
        Ok(e)
    }
}

/// Shared, per-stage immutable data handed to worker threads.
struct StageCtx<'a> {
    model: &'a Model,
    stage: &'a Stage,
    overlay: Arc<Overlay>,
    frozen: Vec<usize>,
    state: Vec<(usize, Cell, Lanes)>,
    n: usize,
    j_lanes: Lanes,
    case_ids: Lanes,
    exprs: HashMap<String, Arc<Expr>>,
}

impl<'a> StageCtx<'a> {
    fn expr(&self, t: &str) -> Arc<Expr> {
        self.exprs[t].clone()
    }
    fn job(&self, lanes: usize) -> Job<'a> {
        let mut job = Job::new(self.model, lanes);
        job.set_overlay(self.overlay.clone());
        for &f in &self.frozen {
            job.freeze_sheet(f);
        }
        for (s, c, v) in &self.state {
            job.set_override(*s, *c, v.clone());
        }
        job
    }
    fn eval(&self, job: &mut Job, text: &str) -> Res<Lanes> {
        job.eval_expr(&self.expr(text), 0).map_err(|e| RunError::Eval(format!("'{text}': {e}")))
    }
    fn eval_uniform(&self, job: &mut Job, text: &str) -> Res<Scalar> {
        let l = self.eval(job, text)?;
        l.as_uniform().cloned().ok_or_else(|| RunError::Eval(format!("'{text}' differs between cases")))
    }
    fn eval_pos(&self, job: &mut Job, text: &str) -> Res<i64> {
        match self.eval_uniform(job, text)?.to_num() {
            Ok(v) => Ok(v as i64),
            Err(e) => Err(RunError::Eval(format!("'{text}' = {e}"))),
        }
    }
    fn truthy(&self, job: &mut Job, text: &Option<String>) -> Res<bool> {
        match text {
            None => Ok(false),
            Some(t) => Ok(self.eval_uniform(job, t)?.to_bool().unwrap_or(false)),
        }
    }
}

struct JobSpec {
    vars: Vec<(String, Scalar)>,
    skip_cases: bool,
}

pub fn run(model: &Model, contract: &Contract, stages: &[String], opts: &RunOptions) -> Res<RunResult> {
    for s in &contract.required_sheets {
        sheet_idx(model, s)?;
    }
    let mut overlay: Overlay = HashMap::new();
    // Workbook state carried between stages (driver values, flags), like the live workbook.
    let mut state: HashMap<(usize, Cell), Lanes> = HashMap::new();
    let mut reports = Vec::new();
    for (si, name) in stages.iter().enumerate() {
        let stage = contract.stage(name).ok_or_else(|| RunError::Contract(format!("no stage '{name}'")))?;
        let started = Instant::now();
        let (patches, report, new_state) = run_stage(model, stage, &overlay, &state, opts, si, stages.len())?;
        for (sheet, p) in patches {
            merge(overlay.entry(sheet).or_default(), p);
        }
        state = new_state;
        reports.push(StageReport { seconds: started.elapsed().as_secs_f64(), ..report });
    }
    let mut final_state: Vec<(usize, Cell, Scalar)> = state.into_iter().map(|((s, c), v)| (s, c, v.get(0))).collect();
    final_state.sort_by_key(|(s, c, _)| (*s, c.row, c.col));
    Ok(RunResult { patches: overlay, reports, final_state })
}

fn merge(dst: &mut SheetPatch, src: SheetPatch) {
    if src.cleared_all {
        *dst = src;
        return;
    }
    for r in &src.cleared {
        for row in r.r1..=r.r2.min(dst.rows.len() as u32) {
            let line = &mut dst.rows[row as usize - 1];
            for col in r.c1..=r.c2.min(line.len() as u32) {
                line[col as usize - 1] = None;
            }
        }
        dst.cleared.push(*r);
    }
    for (ri, line) in src.rows.into_iter().enumerate() {
        for (ci, v) in line.into_iter().enumerate() {
            if let Some(v) = v {
                dst.set(Cell { row: ri as u32 + 1, col: ci as u32 + 1 }, v);
            }
        }
    }
}

#[allow(clippy::type_complexity)]
fn run_stage(
    model: &Model,
    stage: &Stage,
    prior: &Overlay,
    state: &HashMap<(usize, Cell), Lanes>,
    opts: &RunOptions,
    stage_index: usize,
    stage_count: usize,
) -> Res<(Overlay, StageReport, HashMap<(usize, Cell), Lanes>)> {
    // ---- compile every expression of the stage once
    let mut c = Compiled { exprs: HashMap::new() };
    let mut texts: Vec<&str> = Vec::new();
    for [_, f] in stage.set.iter().chain(&stage.after).chain(&stage.vars) {
        texts.push(f);
    }
    for l in &stage.loops {
        texts.push(&l.from);
        texts.push(&l.to);
    }
    texts.extend(stage.skip.iter().map(String::as_str));
    texts.extend(stage.skip_cases.iter().map(String::as_str));
    if let Some(cs) = &stage.cases {
        texts.push(&cs.count);
        texts.push(&cs.id);
    }
    for d in &stage.drivers {
        texts.push(&d.value);
    }
    for o in &stage.outputs {
        texts.push(&o.row);
        texts.push(&o.col);
        texts.extend(o.when.iter().map(String::as_str));
    }
    for h in &stage.headers {
        texts.push(&h.row);
        texts.push(&h.col);
        texts.extend(h.values.iter().map(String::as_str));
        texts.extend(h.when.iter().map(String::as_str));
    }
    for t in texts {
        c.get(t)?;
    }

    // ---- this stage's clears become a patch layered over prior stages' results
    let mut own: Overlay = HashMap::new();
    for cl in &stage.clear {
        if cl.contains('!') {
            let (s, r) = resolve_range(model, cl)?;
            own.entry(s).or_default().cleared.push(r);
        } else {
            own.entry(sheet_idx(model, cl)?).or_default().cleared_all = true;
        }
    }
    let mut layered = prior.clone();
    for (s, p) in &own {
        merge(layered.entry(*s).or_default(), p.clone());
    }

    let mut state_list: Vec<(usize, Cell, Lanes)> = state.iter().map(|((s, c), v)| (*s, *c, v.clone())).collect();
    let mut sctx = StageCtx {
        model,
        stage,
        overlay: Arc::new(layered),
        frozen: stage.freeze.iter().map(|f| sheet_idx(model, f)).collect::<Res<_>>()?,
        state: Vec::new(),
        n: 1,
        j_lanes: Lanes::Uniform(Scalar::Num(1.0)),
        case_ids: Lanes::Uniform(Scalar::Empty),
        exprs: c.exprs,
    };
    // Stage `set` cells (evaluated against the current state) apply for the whole stage.
    sctx.state = state_list.clone();
    let mut probe = sctx.job(1);
    for [cell, f] in &stage.set {
        let (s, r) = resolve_range(model, cell)?;
        let v = sctx.eval(&mut probe, f)?;
        state_list.retain(|(ss, cc, _)| !(*ss == s && *cc == Cell { row: r.r1, col: r.c1 }));
        state_list.push((s, Cell { row: r.r1, col: r.c1 }, v));
    }
    sctx.state = state_list.clone();

    // ---- cases
    let mut probe = sctx.job(1);
    let n = match &stage.cases {
        None => 1,
        Some(cs) => {
            let n = sctx.eval_pos(&mut probe, &cs.count)?.max(0) as usize;
            opts.case_limit.map_or(n, |l| n.min(l))
        }
    };
    sctx.n = n.max(1);
    sctx.j_lanes = Lanes::from_nums((1..=sctx.n).map(|j| j as f64).collect());
    if let Some(cs) = &stage.cases {
        let mut cj = sctx.job(sctx.n);
        cj.bind("J", Value::S(sctx.j_lanes.clone()));
        sctx.case_ids = sctx.eval(&mut cj, &cs.id)?;
    }

    // ---- enumerate jobs (nested loops), job variables and skips
    let mut specs: Vec<JobSpec> = Vec::new();
    let mut ranges = Vec::new();
    for l in &stage.loops {
        let (a, b) = (sctx.eval_pos(&mut probe, &l.from)?, sctx.eval_pos(&mut probe, &l.to)?);
        ranges.push((l.var.clone(), a, b));
    }
    let mut idx: Vec<i64> = ranges.iter().map(|r| r.1).collect();
    if ranges.iter().all(|r| r.1 <= r.2) {
        'outer: loop {
            probe.unbind_all();
            let mut vars: Vec<(String, Scalar)> = ranges.iter().zip(&idx).map(|(r, v)| (r.0.clone(), Scalar::Num(*v as f64))).collect();
            for (k, v) in &vars {
                probe.bind(k, Value::S(Lanes::Uniform(v.clone())));
            }
            for [k, f] in &stage.vars {
                let v = sctx.eval_uniform(&mut probe, f)?;
                probe.bind(k, Value::S(Lanes::Uniform(v.clone())));
                vars.push((k.clone(), v));
            }
            if !sctx.truthy(&mut probe, &stage.skip)? {
                let skip_cases = sctx.truthy(&mut probe, &stage.skip_cases)?;
                specs.push(JobSpec { vars, skip_cases });
            }
            // advance innermost first
            let mut d = ranges.len();
            loop {
                if d == 0 {
                    break 'outer;
                }
                d -= 1;
                idx[d] += 1;
                if idx[d] <= ranges[d].2 {
                    break;
                }
                idx[d] = ranges[d].1;
            }
        }
    }
    let total_jobs = specs.len();

    // ---- static and case headers
    let sheets_touched: Vec<usize> = stage
        .outputs
        .iter()
        .map(|o| sheet_idx(model, &o.sheet))
        .chain(stage.headers.iter().map(|h| sheet_idx(model, &h.sheet)))
        .collect::<Res<_>>()?;
    let patches: HashMap<usize, Mutex<SheetPatch>> = sheets_touched.iter().chain(own.keys()).map(|s| (*s, Mutex::new(own.get(s).cloned().unwrap_or_default()))).collect();
    let excel_errors = AtomicUsize::new(0);
    let written = AtomicUsize::new(0);
    let write = |sheet: usize, cells: Vec<(Cell, CellValue)>| {
        written.fetch_add(cells.len(), Ordering::Relaxed);
        excel_errors.fetch_add(cells.iter().filter(|(_, v)| matches!(v, CellValue::Error(_))).count(), Ordering::Relaxed);
        let mut p = patches[&sheet].lock().unwrap();
        for (c, v) in cells {
            p.set(c, v);
        }
    };
    {
        let mut hj = sctx.job(sctx.n);
        hj.bind("J", Value::S(sctx.j_lanes.clone()));
        hj.bind("CASE_ID", Value::S(sctx.case_ids.clone()));
        hj.bind("CASE_COUNT", Value::num(sctx.n as f64));
        for h in stage.headers.iter().filter(|h| h.scope != Scope::Job) {
            let cells = header_cells(&sctx, &mut hj, h)?;
            write(sheet_idx(model, &h.sheet)?, cells);
        }
    }

    // ---- run jobs in parallel
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let failure: Mutex<Option<RunError>> = Mutex::new(None);
    // Final state: drivers from the last job in loop order, case drivers from the last job that ran cases.
    let last_state: Mutex<HashMap<usize, Vec<(usize, Cell, Lanes)>>> = Mutex::new(HashMap::new());
    let threads = opts.threads.max(1).min(total_jobs.max(1));
    let report_progress = |d: usize| {
        if let Some(p) = &opts.progress {
            p(&Progress { stage: stage.name.clone(), stage_index, stage_count, done: d, total: total_jobs });
        }
    };
    report_progress(0);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            std::thread::Builder::new()
                .stack_size(1 << 30)
                .spawn_scoped(scope, || loop {
                    if opts.cancel.load(Ordering::Relaxed) || failure.lock().unwrap().is_some() {
                        break;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(spec) = specs.get(i) else { break };
                    match run_job(&sctx, spec, &write) {
                        Ok(st) => {
                            last_state.lock().unwrap().insert(i, st);
                        }
                        Err(e) => {
                            let ctx: Vec<String> = spec.vars.iter().map(|(k, v)| format!("{k}={}", show(v))).collect();
                            *failure.lock().unwrap() = Some(match e {
                                RunError::Eval(m) => RunError::Eval(format!("stage '{}', job {}: {m}", stage.name, ctx.join(", "))),
                                other => other,
                            });
                            break;
                        }
                    }
                    report_progress(done.fetch_add(1, Ordering::Relaxed) + 1);
                })
                .expect("spawn worker");
        }
    });
    if let Some(e) = failure.into_inner().unwrap() {
        return Err(e);
    }
    if opts.cancel.load(Ordering::Relaxed) {
        return Err(RunError::Cancelled);
    }

    // ---- workbook state after the stage
    let mut new_state: HashMap<(usize, Cell), Lanes> = state.clone();
    for (s, c, v) in &state_list {
        new_state.insert((*s, *c), v.clone());
    }
    let ls = last_state.into_inner().unwrap();
    let mut order: Vec<usize> = ls.keys().copied().collect();
    order.sort();
    for i in order {
        for (s, c, v) in &ls[&i] {
            new_state.insert((*s, *c), v.clone());
        }
    }
    let mut probe = sctx.job(1);
    for [cell, f] in &stage.after {
        let (s, r) = resolve_range(model, cell)?;
        let v = sctx.eval(&mut probe, f)?;
        new_state.insert((s, Cell { row: r.r1, col: r.c1 }), v);
    }
    let out: Overlay = patches.into_iter().map(|(s, m)| (s, m.into_inner().unwrap())).collect();
    let report = StageReport {
        name: stage.name.clone(),
        jobs: total_jobs,
        active_jobs: specs.iter().filter(|s| !s.skip_cases).count(),
        cases: sctx.n,
        seconds: 0.0,
        cells_written: written.into_inner(),
        excel_errors: excel_errors.into_inner(),
    };
    Ok((out, report, new_state))
}

fn show(s: &Scalar) -> String {
    match s {
        Scalar::Num(n) => xlcalc::scalar::number_to_text(*n),
        Scalar::Text(t) => format!("\"{t}\""),
        other => format!("{other:?}"),
    }
}

fn header_cells(sctx: &StageCtx, job: &mut Job, h: &Header) -> Res<Vec<(Cell, CellValue)>> {
    if sctx.truthy_opt(job, &h.when)? == Some(false) {
        return Ok(vec![]);
    }
    let base = (sctx.eval_pos(job, &h.row)?, sctx.eval_pos(job, &h.col)?);
    let mut out = Vec::new();
    for (k, f) in h.values.iter().enumerate() {
        let v = sctx.eval(job, f)?;
        if h.scope == Scope::Case {
            for j in 0..sctx.n {
                if let Some(c) = place(base, h.step, k, h.case_step.or(Some([0, 1])), j) {
                    out.push((c, to_cell(&v.get(j))));
                }
            }
        } else {
            let s = v.as_uniform().cloned().ok_or_else(|| RunError::Eval(format!("header '{f}' differs between cases")))?;
            if let Some(c) = place(base, h.step, k, None, 0) {
                out.push((c, to_cell(&s)));
            }
        }
    }
    Ok(out)
}

impl StageCtx<'_> {
    fn truthy_opt(&self, job: &mut Job, text: &Option<String>) -> Res<Option<bool>> {
        match text {
            None => Ok(None),
            Some(t) => Ok(Some(self.eval_uniform(job, t)?.to_bool().unwrap_or(false))),
        }
    }
}

fn run_job(sctx: &StageCtx, spec: &JobSpec, write: &(dyn Fn(usize, Vec<(Cell, CellValue)>) + Sync)) -> Res<Vec<(usize, Cell, Lanes)>> {
    let model = sctx.model;
    let stage = sctx.stage;
    let mut job = sctx.job(sctx.n);
    for (k, v) in &spec.vars {
        job.bind(k, Value::S(Lanes::Uniform(v.clone())));
    }
    job.bind("J", Value::S(sctx.j_lanes.clone()));
    job.bind("CASE_ID", Value::S(sctx.case_ids.clone()));
    job.bind("CASE_COUNT", Value::num(sctx.n as f64));
    let mut final_state = Vec::new();
    for d in &stage.drivers {
        let (s, r) = resolve_range(model, &d.cell)?;
        let v = sctx.eval(&mut job, &d.value)?;
        let cell = Cell { row: r.r1, col: r.c1 };
        if d.per_case {
            if !spec.skip_cases {
                final_state.push((s, cell, Lanes::Uniform(v.get(sctx.n - 1))));
            }
        } else {
            final_state.push((s, cell, v.clone()));
        }
        job.set_override(s, cell, v);
    }
    // Per-sheet batches of writes.
    let mut batches: HashMap<usize, Vec<(Cell, CellValue)>> = HashMap::new();
    for h in stage.headers.iter().filter(|h| h.scope == Scope::Job) {
        let cells = header_cells(sctx, &mut job, h)?;
        batches.entry(sheet_idx(model, &h.sheet)?).or_default().extend(cells);
    }
    for o in &stage.outputs {
        if spec.skip_cases && !o.ignore_skip_cases {
            continue;
        }
        if sctx.truthy_opt(&mut job, &o.when)? == Some(false) {
            continue;
        }
        let (src_sheet, rect) = resolve_range(model, &o.source)?;
        let target = sheet_idx(model, &o.sheet)?;
        let base = (sctx.eval_pos(&mut job, &o.row)?, sctx.eval_pos(&mut job, &o.col)?);
        let out = batches.entry(target).or_default();
        let mut k = 0;
        for row in rect.r1..=rect.r2 {
            for col in rect.c1..=rect.c2 {
                let lanes = job.cell(src_sheet, Cell { row, col }).map_err(|e| RunError::Eval(format!("{}: {e}", o.source)))?;
                match o.case_step {
                    Some(cs) => {
                        for j in 0..sctx.n {
                            if let Some(c) = place(base, o.step, k, Some(cs), j) {
                                out.push((c, to_cell(&lanes.get(j))));
                            }
                        }
                    }
                    None => {
                        let s = lanes
                            .as_uniform()
                            .cloned()
                            .ok_or_else(|| RunError::Eval(format!("{} depends on the case but is written once", o.source)))?;
                        if let Some(c) = place(base, o.step, k, None, 0) {
                            out.push((c, to_cell(&s)));
                        }
                    }
                }
                k += 1;
            }
        }
    }
    for (s, cells) in batches {
        write(s, cells);
    }
    Ok(final_state)
}
