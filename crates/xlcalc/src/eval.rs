//! Lazy, memoised, lane-vectorised evaluation of Excel formulas.
use crate::lanes::{zip2, zip2_num, Lanes};
use crate::scalar::{compare, number_to_text, Scalar};
use crate::value::{Array, RefRange, Value};
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use xlformula::{Area, BinOp, Coord, Expr, Reference, UnOp, MAX_COL, MAX_ROW};
use xlmodel::{Cell, FormulaKind, Rect, Workbook, XlError};

#[derive(Debug, Clone, thiserror::Error)]
pub enum EvalError {
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("circular reference at {0}")]
    Circular(String),
    #[error("{0}")]
    Other(String),
}

pub type R<T> = Result<T, EvalError>;

pub fn unsupported<T>(msg: impl Into<String>) -> R<T> {
    Err(EvalError::Unsupported(msg.into()))
}

/// Immutable, thread-shareable compiled workbook.
pub struct Model {
    pub wb: Workbook,
    exprs: Vec<HashMap<Cell, Arc<Expr>>>,
    names: HashMap<(Option<usize>, String), Arc<Expr>>,
    /// Cells whose value is computed (formula cells and array members), per sheet, as (row, col).
    computed: Vec<BTreeSet<(u32, u32)>>,
    /// Used extent per sheet (rows, cols) including formulas.
    extent: Vec<(u32, u32)>,
}

impl Model {
    pub fn new(wb: Workbook) -> Result<Self, String> {
        let mut exprs = Vec::new();
        let mut computed = Vec::new();
        let mut extent = Vec::new();
        let mut errors = Vec::new();
        for s in &wb.sheets {
            let mut groups: HashMap<(Cell, *const u8), Arc<Expr>> = HashMap::new();
            let mut map = HashMap::new();
            let mut set = BTreeSet::new();
            let (mut mr, mut mc) = (s.max_row(), s.max_col());
            for (cell, f) in &s.formulas {
                let key = (f.origin, f.text.as_ptr());
                let e = match groups.get(&key) {
                    Some(e) => e.clone(),
                    None => match xlformula::parse(&f.text, f.origin) {
                        Ok(e) => {
                            let e = Arc::new(e);
                            groups.insert(key, e.clone());
                            e
                        }
                        Err(err) => {
                            // Keep going: only a reachable unparsable formula is fatal.
                            if errors.len() < 20 {
                                errors.push(format!("{}!{}: {err}", s.name, xlmodel::a1(*cell)));
                            }
                            Arc::new(Expr::Call(Arc::from("_BC_UNPARSABLE"), vec![]))
                        }
                    },
                };
                map.insert(*cell, e);
                set.insert((cell.row, cell.col));
                mr = mr.max(cell.row);
                mc = mc.max(cell.col);
            }
            for (cell, _) in &s.array_members {
                set.insert((cell.row, cell.col));
                mr = mr.max(cell.row);
                mc = mc.max(cell.col);
            }
            exprs.push(map);
            computed.push(set);
            extent.push((mr, mc));
        }
        let mut names = HashMap::new();
        for n in &wb.names {
            if let Ok(e) = xlformula::parse(&n.refers_to, Cell { row: 1, col: 1 }) {
                names.insert((n.scope, n.name.to_ascii_uppercase()), Arc::new(e));
            }
        }
        Ok(Model { wb, exprs, names, computed, extent })
    }

    pub fn sheet_index(&self, name: &str) -> Option<usize> {
        self.wb.sheet_index(name)
    }
    pub fn extent(&self, sheet: usize) -> (u32, u32) {
        self.extent[sheet]
    }
    /// True when no computed cell lies inside `r` on `sheet`.
    pub fn is_constant_range(&self, sheet: usize, r: &Rect) -> bool {
        let set = &self.computed[sheet];
        if r.rows() as usize > set.len() {
            !set.iter().any(|&(row, col)| r.contains(Cell { row, col }))
        } else {
            (r.r1..=r.r2).all(|row| set.range((row, r.c1)..=(row, r.c2)).next().is_none())
        }
    }
    fn name_expr(&self, name: &str, sheet: Option<usize>) -> Option<Arc<Expr>> {
        if let Some(s) = sheet {
            if let Some(e) = self.names.get(&(Some(s), name.to_string())) {
                return Some(e.clone());
            }
        }
        self.names.get(&(None, name.to_string())).cloned()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Ctx {
    pub sheet: usize,
    pub cell: Cell,
    /// Array (CSE / dynamic array) evaluation: references are not implicitly intersected.
    pub array: bool,
}

/// One batch evaluation over `n` lanes. Not shared between threads.
pub struct Job<'m> {
    pub m: &'m Model,
    pub n: usize,
    overrides: HashMap<(usize, Cell), Lanes>,
    memo: HashMap<(usize, Cell), Lanes>,
    anchors: HashMap<(usize, Cell), Arc<Array>>,
    active: HashSet<(usize, Cell)>,
    pub(crate) env: Vec<(Arc<str>, Value)>,
    range_memo: HashMap<RefRange, Arc<Array>>,
    pub(crate) lookup_cache: HashMap<(RefRange, u8), Arc<crate::lookup::Index>>,
    /// Sheets whose formulas are frozen at their saved values (Worksheet.EnableCalculation=False).
    frozen: HashSet<usize>,
    cleared: HashSet<usize>,
    pub evaluated_cells: usize,
}

impl<'m> Job<'m> {
    pub fn new(m: &'m Model, n: usize) -> Self {
        Job {
            m,
            n,
            overrides: HashMap::new(),
            memo: HashMap::new(),
            anchors: HashMap::new(),
            active: HashSet::new(),
            env: Vec::new(),
            range_memo: HashMap::new(),
            lookup_cache: HashMap::new(),
            frozen: HashSet::new(),
            cleared: HashSet::new(),
            evaluated_cells: 0,
        }
    }

    pub fn set_override(&mut self, sheet: usize, cell: Cell, v: Lanes) {
        self.overrides.insert((sheet, cell), v);
    }
    pub fn freeze_sheet(&mut self, sheet: usize) {
        self.frozen.insert(sheet);
    }
    /// Treat every cell of a sheet as empty (e.g. outputs cleared before a macro loop).
    pub fn clear_sheet(&mut self, sheet: usize) {
        self.frozen.insert(sheet);
        self.cleared.insert(sheet);
    }
    /// Provide externally computed values for a sheet region (e.g. outputs of a previous stage).
    pub fn set_values(&mut self, sheet: usize, cells: impl IntoIterator<Item = (Cell, Lanes)>) {
        for (c, l) in cells {
            self.overrides.insert((sheet, c), l);
        }
    }

    fn cell_name(&self, sheet: usize, c: Cell) -> String {
        format!("{}!{}", self.m.wb.sheets[sheet].name, xlmodel::a1(c))
    }

    /// Value of one cell across all lanes.
    pub fn cell(&mut self, sheet: usize, c: Cell) -> R<Lanes> {
        if let Some(v) = self.overrides.get(&(sheet, c)) {
            return Ok(v.clone());
        }
        if let Some(v) = self.memo.get(&(sheet, c)) {
            return Ok(v.clone());
        }
        let sh = &self.m.wb.sheets[sheet];
        let frozen = self.frozen.contains(&sheet);
        if !frozen {
            if let Some(&anchor) = sh.array_members.get(&c) {
                let arr = self.anchor(sheet, anchor)?;
                let FormulaKind::Array { range, dynamic } = sh.formulas[&anchor].kind else { unreachable!() };
                let (r, col) = ((c.row - range.r1) as usize, (c.col - range.c1) as usize);
                let v = if dynamic {
                    if r < arr.rows && col < arr.cols { arr.get(r, col).clone() } else { Lanes::Uniform(Scalar::Empty) }
                } else {
                    arr.at_broadcast(r, col)
                };
                self.memo.insert((sheet, c), v.clone());
                return Ok(v);
            }
            if let Some(e) = self.m.exprs[sheet].get(&c).cloned() {
                if !self.active.insert((sheet, c)) {
                    return Err(EvalError::Circular(self.cell_name(sheet, c)));
                }
                let cx = Ctx { sheet, cell: c, array: false };
                let res = self.eval(&e, &cx).and_then(|v| self.to_lanes(v, &cx));
                self.active.remove(&(sheet, c));
                let v = res.map_err(|e| self.wrap(e, sheet, c))?;
                let v = v.map(self.n, |s| if *s == Scalar::Empty { Scalar::Num(0.0) } else { s.clone() });
                self.evaluated_cells += 1;
                self.memo.insert((sheet, c), v.clone());
                return Ok(v);
            }
        }
        if self.cleared.contains(&sheet) {
            return Ok(Lanes::Uniform(Scalar::Empty));
        }
        Ok(Lanes::Uniform(Scalar::from(sh.value(c))))
    }

    fn wrap(&self, e: EvalError, sheet: usize, c: Cell) -> EvalError {
        match e {
            EvalError::Unsupported(m) if !m.contains(" <- ") => EvalError::Unsupported(format!("{m} <- {}", self.cell_name(sheet, c))),
            other => other,
        }
    }

    /// Evaluate an array formula anchor to its full result array.
    fn anchor(&mut self, sheet: usize, a: Cell) -> R<Arc<Array>> {
        if let Some(v) = self.anchors.get(&(sheet, a)) {
            return Ok(v.clone());
        }
        let e = self.m.exprs[sheet].get(&a).cloned().ok_or_else(|| EvalError::Other("array anchor without formula".into()))?;
        if !self.active.insert((sheet, a)) {
            return Err(EvalError::Circular(self.cell_name(sheet, a)));
        }
        let cx = Ctx { sheet, cell: a, array: true };
        let res = self.eval(&e, &cx).and_then(|v| self.to_array(v));
        self.active.remove(&(sheet, a));
        let arr = res.map_err(|e| self.wrap(e, sheet, a))?;
        let n = self.n;
        let arr = Arc::new(Array::new(
            arr.rows,
            arr.cols,
            arr.data.iter().map(|l| l.map(n, |s| if *s == Scalar::Empty { Scalar::Num(0.0) } else { s.clone() })).collect(),
        ));
        self.evaluated_cells += 1;
        self.anchors.insert((sheet, a), arr.clone());
        Ok(arr)
    }

    // ---------------------------------------------------------------- references

    pub fn resolve_ref(&self, r: &Reference, cx: &Ctx) -> Value {
        let sheet = match &r.sheet {
            None => cx.sheet,
            Some(name) => match self.m.sheet_index(name) {
                Some(s) => s,
                None => return Value::err(XlError::Ref),
            },
        };
        let row = |c: Coord| c.at(cx.cell.row, MAX_ROW);
        let col = |c: Coord| c.at(cx.cell.col, MAX_COL);
        let rect = match r.area {
            Area::Cell { row: rr, col: cc } => match (row(rr), col(cc)) {
                (Some(a), Some(b)) => Rect { r1: a, c1: b, r2: a, c2: b },
                _ => return Value::err(XlError::Ref),
            },
            Area::Range { r1, c1, r2, c2 } => match (row(r1), col(c1), row(r2), col(c2)) {
                (Some(a), Some(b), Some(c), Some(d)) => Rect { r1: a.min(c), c1: b.min(d), r2: a.max(c), c2: b.max(d) },
                _ => return Value::err(XlError::Ref),
            },
            Area::Cols { c1, c2 } => match (col(c1), col(c2)) {
                (Some(a), Some(b)) => Rect { r1: 1, c1: a.min(b), r2: MAX_ROW, c2: a.max(b) },
                _ => return Value::err(XlError::Ref),
            },
            Area::Rows { r1, r2 } => match (row(r1), row(r2)) {
                (Some(a), Some(b)) => Rect { r1: a.min(b), c1: 1, r2: a.max(b), c2: MAX_COL },
                _ => return Value::err(XlError::Ref),
            },
        };
        Value::R(RefRange { sheet, rect })
    }

    /// Materialise a reference as an array, clamped to the sheet's used extent.
    pub fn deref(&mut self, r: &RefRange) -> R<Arc<Array>> {
        if let Some(a) = self.range_memo.get(r) {
            return Ok(a.clone());
        }
        let (mr, mc) = self.m.extent(r.sheet);
        let r2 = r.rect.r2.min(mr.max(r.rect.r1));
        let c2 = r.rect.c2.min(mc.max(r.rect.c1));
        let (rows, cols) = ((r2 - r.rect.r1 + 1) as usize, (c2 - r.rect.c1 + 1) as usize);
        if rows * cols > 20_000_000 {
            return unsupported(format!("range too large ({rows}x{cols})"));
        }
        let mut data = Vec::with_capacity(rows * cols);
        for row in r.rect.r1..=r2 {
            for col in r.rect.c1..=c2 {
                data.push(self.cell(r.sheet, Cell { row, col })?);
            }
        }
        let a = Arc::new(Array::new(rows, cols, data));
        self.range_memo.insert(*r, a.clone());
        Ok(a)
    }

    /// Shape of a reference without clamping.
    pub fn ref_shape(r: &RefRange) -> (usize, usize) {
        (r.rect.rows() as usize, r.rect.cols() as usize)
    }

    /// Implicit intersection of a reference with the formula cell (legacy semantics).
    fn intersect(&mut self, r: &RefRange, cx: &Ctx) -> R<Lanes> {
        let rect = r.rect;
        let c = if rect.r1 == rect.r2 && rect.c1 == rect.c2 {
            Some(Cell { row: rect.r1, col: rect.c1 })
        } else if rect.c1 == rect.c2 && (rect.r1..=rect.r2).contains(&cx.cell.row) {
            Some(Cell { row: cx.cell.row, col: rect.c1 })
        } else if rect.r1 == rect.r2 && (rect.c1..=rect.c2).contains(&cx.cell.col) {
            Some(Cell { row: rect.r1, col: cx.cell.col })
        } else {
            None
        };
        match c {
            Some(c) => self.cell(r.sheet, c),
            None => Ok(Lanes::Uniform(Scalar::Err(XlError::Value))),
        }
    }

    /// Lane-varying references dereferenced to an array (all lanes must share a shape).
    pub fn deref_lanes(&mut self, refs: &[Result<RefRange, XlError>]) -> R<Arc<Array>> {
        let mut shape = None;
        for r in refs.iter().flatten() {
            let s = Self::ref_shape(r);
            match shape {
                None => shape = Some(s),
                Some(p) if p != s => return unsupported("case-dependent reference with case-dependent shape"),
                _ => {}
            }
        }
        let Some((rows, cols)) = shape else {
            // Every lane is an error.
            let v = Lanes::from_vec(refs.iter().map(|r| Scalar::Err(*r.as_ref().unwrap_err())).collect());
            return Ok(Arc::new(Array::scalar(v)));
        };
        if rows * cols > 1_000_000 {
            return unsupported("case-dependent reference too large");
        }
        let mut data = Vec::with_capacity(rows * cols);
        for dr in 0..rows as u32 {
            for dc in 0..cols as u32 {
                let mut lanes = Vec::with_capacity(self.n);
                for (i, r) in refs.iter().enumerate() {
                    lanes.push(match r {
                        Ok(r) => self.cell(r.sheet, Cell { row: r.rect.r1 + dr, col: r.rect.c1 + dc })?.get(i),
                        Err(e) => Scalar::Err(*e),
                    });
                }
                data.push(Lanes::from_vec(lanes));
            }
        }
        Ok(Arc::new(Array::new(rows, cols, data)))
    }

    /// Build a lane-varying reference value, collapsing to a plain reference when uniform.
    pub fn lane_refs(&self, refs: Vec<Result<RefRange, XlError>>) -> Value {
        if refs.iter().skip(1).all(|r| *r == refs[0]) {
            return match refs[0] {
                Ok(r) => Value::R(r),
                Err(e) => Value::err(e),
            };
        }
        Value::LR(Arc::new(refs))
    }

    // ---------------------------------------------------------------- conversions

    /// Scalar context: references are implicitly intersected, arrays give their top-left.
    pub fn to_lanes(&mut self, v: Value, cx: &Ctx) -> R<Lanes> {
        match v {
            Value::S(l) => Ok(l),
            Value::A(a) => Ok(a.data.first().cloned().unwrap_or(Lanes::Uniform(Scalar::Empty))),
            Value::R(r) => self.intersect(&r, cx),
            Value::LR(refs) => {
                let mut out = Vec::with_capacity(self.n);
                for (i, r) in refs.iter().enumerate() {
                    out.push(match r {
                        Ok(r) if r.is_cell() => self.cell(r.sheet, r.top_left())?.get(i),
                        Ok(r) => self.intersect(r, cx)?.get(i),
                        Err(e) => Scalar::Err(*e),
                    });
                }
                Ok(Lanes::from_vec(out))
            }
        }
    }

    /// Array context: references are materialised.
    pub fn to_array(&mut self, v: Value) -> R<Arc<Array>> {
        match v {
            Value::S(l) => Ok(Arc::new(Array::scalar(l))),
            Value::A(a) => Ok(a),
            Value::R(r) => self.deref(&r),
            Value::LR(refs) => self.deref_lanes(&refs),
        }
    }

    /// Operand of an element-wise operation: scalar lanes or an array.
    pub fn operand(&mut self, v: Value, cx: &Ctx) -> R<Value> {
        match v {
            Value::R(_) | Value::LR(_) if !cx.array => Ok(Value::S(self.to_lanes(v, cx)?)),
            Value::R(_) | Value::LR(_) => {
                let a = self.to_array(v)?;
                Ok(if a.rows == 1 && a.cols == 1 { Value::S(a.data[0].clone()) } else { Value::A(a) })
            }
            other => Ok(other),
        }
    }

    /// Element-wise lifting of an N-ary lane function over scalar/array operands.
    pub fn lift(&mut self, args: Vec<Value>, cx: &Ctx, f: &dyn Fn(&[&Lanes]) -> Lanes) -> R<Value> {
        let mut ops = Vec::with_capacity(args.len());
        for a in args {
            ops.push(self.operand(a, cx)?);
        }
        let arrays: Vec<&Arc<Array>> = ops.iter().filter_map(|o| if let Value::A(a) = o { Some(a) } else { None }).collect();
        if arrays.is_empty() {
            let ls: Vec<&Lanes> = ops.iter().map(|o| if let Value::S(l) = o { l } else { unreachable!() }).collect();
            return Ok(Value::S(f(&ls)));
        }
        let rows = arrays.iter().map(|a| a.rows).max().unwrap();
        let cols = arrays.iter().map(|a| a.cols).max().unwrap();
        let mut data = Vec::with_capacity(rows * cols);
        for r in 0..rows {
            for c in 0..cols {
                let elems: Vec<Lanes> = ops
                    .iter()
                    .map(|o| match o {
                        Value::S(l) => l.clone(),
                        Value::A(a) => a.at_broadcast(r, c),
                        _ => unreachable!(),
                    })
                    .collect();
                let refs: Vec<&Lanes> = elems.iter().collect();
                data.push(f(&refs));
            }
        }
        Ok(Value::A(Arc::new(Array::new(rows, cols, data))))
    }

    // ---------------------------------------------------------------- expressions

    pub fn eval(&mut self, e: &Expr, cx: &Ctx) -> R<Value> {
        Ok(match e {
            Expr::Number(n) => Value::num(*n),
            Expr::Text(t) => Value::S(Lanes::Uniform(Scalar::Text(t.clone()))),
            Expr::Bool(b) => Value::S(Lanes::Uniform(Scalar::Bool(*b))),
            Expr::Error(err) => Value::err(*err),
            Expr::Missing => Value::S(Lanes::Uniform(Scalar::Empty)),
            Expr::RefError => Value::err(XlError::Ref),
            Expr::Array(rows) => {
                let mut data = Vec::new();
                for row in rows {
                    for x in row {
                        let v = self.eval(x, cx)?;
                        data.push(self.to_lanes(v, cx)?);
                    }
                }
                Value::A(Arc::new(Array::new(rows.len(), rows[0].len(), data)))
            }
            Expr::Ref(r) => self.resolve_ref(r, cx),
            Expr::Name(name) => self.eval_name(name, cx)?,
            Expr::Call(name, args) => self.call(name, args, cx)?,
            Expr::Unary(op, x) => self.unary(*op, x, cx)?,
            Expr::Binary(op, a, b) => self.binary(*op, a, b, cx)?,
        })
    }

    fn eval_name(&mut self, name: &str, cx: &Ctx) -> R<Value> {
        if let Some((_, v)) = self.env.iter().rev().find(|(n, _)| &**n == name) {
            return Ok(v.clone());
        }
        let (scope, bare) = match name.rsplit_once('!') {
            Some((s, n)) => (self.m.sheet_index(s), n),
            None => (Some(cx.sheet), name),
        };
        match self.m.name_expr(bare, scope) {
            Some(e) => {
                // Names are evaluated in the caller's context (refers_to is mostly absolute).
                let v = self.eval(&e, cx)?;
                Ok(v)
            }
            None => Ok(Value::err(XlError::Name)),
        }
    }

    fn unary(&mut self, op: UnOp, x: &Expr, cx: &Ctx) -> R<Value> {
        let v = self.eval(x, cx)?;
        let n = self.n;
        match op {
            UnOp::Neg => self.lift(vec![v], cx, &|a| a[0].map_num(n, |x| Ok(-x))),
            UnOp::Plus => Ok(v),
            UnOp::Percent => self.lift(vec![v], cx, &|a| a[0].map_num(n, |x| Ok(x / 100.0))),
            UnOp::Implicit => match v {
                Value::R(_) | Value::LR(_) => Ok(Value::S(self.to_lanes(v, &Ctx { array: false, ..*cx })?)),
                Value::A(a) => Ok(Value::S(a.data[0].clone())),
                s => Ok(s),
            },
            UnOp::Spill => match v {
                Value::R(r) if r.is_cell() => {
                    let a = r.top_left();
                    let sh = &self.m.wb.sheets[r.sheet];
                    if !matches!(sh.formulas.get(&a).map(|f| &f.kind), Some(FormulaKind::Array { .. })) {
                        return Ok(Value::err(XlError::Ref));
                    }
                    let arr = self.anchor(r.sheet, a)?;
                    Ok(Value::R(RefRange {
                        sheet: r.sheet,
                        rect: Rect { r1: a.row, c1: a.col, r2: a.row + arr.rows as u32 - 1, c2: a.col + arr.cols as u32 - 1 },
                    }))
                }
                _ => unsupported("spill operator on non-anchor"),
            },
        }
    }

    fn binary(&mut self, op: BinOp, a: &Expr, b: &Expr, cx: &Ctx) -> R<Value> {
        let n = self.n;
        let va = self.eval(a, cx)?;
        let vb = self.eval(b, cx)?;
        match op {
            BinOp::Range | BinOp::Intersect => {
                let (Value::R(x), Value::R(y)) = (&va, &vb) else {
                    if matches!(va, Value::S(ref l) if l.any_err()) {
                        return Ok(va);
                    }
                    if matches!(vb, Value::S(ref l) if l.any_err()) {
                        return Ok(vb);
                    }
                    return unsupported("range operator on case-dependent or non-reference operands");
                };
                if x.sheet != y.sheet {
                    return Ok(Value::err(XlError::Value));
                }
                let (p, q) = (x.rect, y.rect);
                if op == BinOp::Range {
                    Ok(Value::R(RefRange {
                        sheet: x.sheet,
                        rect: Rect { r1: p.r1.min(q.r1), c1: p.c1.min(q.c1), r2: p.r2.max(q.r2), c2: p.c2.max(q.c2) },
                    }))
                } else {
                    let r = Rect { r1: p.r1.max(q.r1), c1: p.c1.max(q.c1), r2: p.r2.min(q.r2), c2: p.c2.min(q.c2) };
                    if r.r1 > r.r2 || r.c1 > r.c2 { Ok(Value::err(XlError::Null)) } else { Ok(Value::R(RefRange { sheet: x.sheet, rect: r })) }
                }
            }
            BinOp::Add => self.lift(vec![va, vb], cx, &|v| zip2_num(v[0], v[1], n, |x, y| x + y, |x, y| arith(x, y, |p, q| Ok(p + q)))),
            BinOp::Sub => self.lift(vec![va, vb], cx, &|v| zip2_num(v[0], v[1], n, |x, y| x - y, |x, y| arith(x, y, |p, q| Ok(p - q)))),
            BinOp::Mul => self.lift(vec![va, vb], cx, &|v| zip2_num(v[0], v[1], n, |x, y| x * y, |x, y| arith(x, y, |p, q| Ok(p * q)))),
            BinOp::Div => self.lift(vec![va, vb], cx, &|v| {
                zip2_num(v[0], v[1], n, |x, y| if y == 0.0 { f64::NAN } else { x / y }, |x, y| arith(x, y, |p, q| if q == 0.0 { Err(XlError::Div0) } else { Ok(p / q) }))
            }),
            BinOp::Pow => self.lift(vec![va, vb], cx, &|v| zip2(v[0], v[1], n, |x, y| arith(x, y, power))),
            BinOp::Concat => self.lift(vec![va, vb], cx, &|v| {
                zip2(v[0], v[1], n, |x, y| match (x.to_text(), y.to_text()) {
                    (Err(e), _) | (_, Err(e)) => Scalar::Err(e),
                    (Ok(p), Ok(q)) => {
                        let mut s = String::with_capacity(p.len() + q.len());
                        s.push_str(&p);
                        s.push_str(&q);
                        Scalar::text(s)
                    }
                })
            }),
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => self.lift(vec![va, vb], cx, &|v| {
                zip2(v[0], v[1], n, |x, y| {
                    if let Scalar::Err(e) = x {
                        return Scalar::Err(*e);
                    }
                    if let Scalar::Err(e) = y {
                        return Scalar::Err(*e);
                    }
                    let o = compare(x, y);
                    Scalar::Bool(match op {
                        BinOp::Eq => o == Ordering::Equal,
                        BinOp::Ne => o != Ordering::Equal,
                        BinOp::Lt => o == Ordering::Less,
                        BinOp::Le => o != Ordering::Greater,
                        BinOp::Gt => o == Ordering::Greater,
                        _ => o != Ordering::Less,
                    })
                })
            }),
        }
    }
}

/// Arithmetic with Excel coercion and error precedence (left operand's error first).
pub fn arith(x: &Scalar, y: &Scalar, f: impl Fn(f64, f64) -> Result<f64, XlError>) -> Scalar {
    let p = match x.to_num() {
        Ok(v) => v,
        Err(e) => return Scalar::Err(e),
    };
    let q = match y.to_num() {
        Ok(v) => v,
        Err(e) => return Scalar::Err(e),
    };
    match f(p, q) {
        Ok(v) => Scalar::num(v),
        Err(e) => Scalar::Err(e),
    }
}

pub fn power(x: f64, y: f64) -> Result<f64, XlError> {
    if x == 0.0 && y == 0.0 {
        return Err(XlError::Num);
    }
    if x == 0.0 && y < 0.0 {
        return Err(XlError::Div0);
    }
    let v = x.powf(y);
    if v.is_nan() {
        // Negative base with an odd-root fraction like -8^(1/3) is #NUM! in Excel too.
        return Err(XlError::Num);
    }
    Ok(v)
}

pub fn general_text(n: f64) -> String {
    number_to_text(n)
}
