//! Reference, lookup and conditional-aggregate functions.
use crate::eval::{unsupported, Ctx, Job, R};
use crate::lanes::Lanes;
use crate::lookup::{approx, has_wildcard, lookup_eq, Criterion, Index};
use crate::scalar::{compare, Scalar};
use crate::value::{Array, RefRange, Value};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;
use xlformula::Expr;
use xlmodel::{Cell, Rect, XlError};

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Mode {
    /// Exact; `wild` enables wildcards; `last` searches from the end.
    Exact { wild: bool, last: bool },
    /// Exact or next smaller / larger on unsorted data (XLOOKUP/XMATCH -1/1).
    NextSmaller,
    NextLarger,
    /// Binary search ascending (MATCH 1, VLOOKUP TRUE) or descending (MATCH -1).
    SortedAsc,
    SortedDesc,
}

type Pos = Result<usize, XlError>;

impl<'m> Job<'m> {
    pub(crate) fn call_ref(&mut self, name: &str, args: &[Expr], cx: &Ctx) -> R<Option<Value>> {
        Ok(Some(match name {
            "ROW" | "COLUMN" => self.f_rowcol(name == "ROW", args, cx)?,
            "ROWS" | "COLUMNS" => {
                if args.len() != 1 {
                    return Ok(Some(Value::err(XlError::Value)));
                }
                let v = self.eval(&args[0], cx)?;
                let (r, c) = match &v {
                    Value::R(r) => Self::ref_shape(r),
                    Value::A(a) => (a.rows, a.cols),
                    Value::S(_) => (1, 1),
                    Value::LR(refs) => match refs.iter().flatten().next() {
                        Some(r) => Self::ref_shape(r),
                        None => return Ok(Some(Value::err(XlError::Ref))),
                    },
                };
                Value::num((if name == "ROWS" { r } else { c }) as f64)
            }
            "INDEX" => self.f_index(args, cx)?,
            "MATCH" => self.f_match(args, cx, false)?,
            "XMATCH" => self.f_match(args, cx, true)?,
            "XLOOKUP" => self.f_xlookup(args, cx)?,
            "VLOOKUP" | "HLOOKUP" => self.f_vhlookup(name == "VLOOKUP", args, cx)?,
            "LOOKUP" => self.f_lookup(args, cx)?,
            "OFFSET" => self.f_offset(args, cx)?,
            "INDIRECT" => self.f_indirect(args, cx)?,
            "ADDRESS" => self.f_address(args, cx)?,
            "COUNTIF" | "COUNTIFS" | "SUMIF" | "SUMIFS" | "AVERAGEIF" | "AVERAGEIFS" | "MINIFS" | "MAXIFS" => self.f_ifs_agg(name, args, cx)?,
            _ => return Ok(None),
        }))
    }

    fn f_rowcol(&mut self, row: bool, args: &[Expr], cx: &Ctx) -> R<Value> {
        let pick = |c: Cell| (if row { c.row } else { c.col }) as f64;
        if args.is_empty() || matches!(args[0], Expr::Missing) {
            return Ok(Value::num(pick(cx.cell)));
        }
        match self.eval(&args[0], cx)? {
            Value::R(r) => {
                let (n, start) = if row { (r.rect.rows(), r.rect.r1) } else { (r.rect.cols(), r.rect.c1) };
                if cx.array && n > 1 {
                    let data: Vec<Lanes> = (0..n).map(|k| Lanes::Uniform(Scalar::Num((start + k) as f64))).collect();
                    let (rows, cols) = if row { (n as usize, 1) } else { (1, n as usize) };
                    return Ok(Value::A(Arc::new(Array::new(rows, cols, data))));
                }
                Ok(Value::num(start as f64))
            }
            Value::LR(refs) => Ok(Value::S(Lanes::from_vec(
                refs.iter().map(|r| match r {
                    Ok(r) => Scalar::Num(pick(r.top_left())),
                    Err(e) => Scalar::Err(*e),
                })
                .collect(),
            ))),
            Value::S(l) if l.any_err() => Ok(Value::S(l)),
            _ => Ok(Value::err(XlError::Value)),
        }
    }

    // ------------------------------------------------------------------ INDEX

    fn f_index(&mut self, args: &[Expr], cx: &Ctx) -> R<Value> {
        if args.len() < 2 || args.len() > 4 {
            return Ok(Value::err(XlError::Value));
        }
        if args.len() == 4 && !matches!(args[3], Expr::Number(n) if n == 1.0) {
            return unsupported("INDEX area_num");
        }
        let src = self.eval(&args[0], cx)?;
        let num = |job: &mut Self, e: Option<&Expr>| -> R<Option<Value>> {
            match e {
                None | Some(Expr::Missing) => Ok(None),
                Some(e) => {
                    let v = job.eval(e, cx)?;
                    Ok(Some(job.operand(v, cx)?))
                }
            }
        };
        let rv = num(self, args.get(1))?;
        let cv = num(self, args.get(2))?;
        if matches!(rv, Some(Value::A(_))) || matches!(cv, Some(Value::A(_))) {
            return self.index_array_args(src, rv, cv, cx);
        }
        let as_lanes = |v: Option<Value>| match v {
            Some(Value::S(l)) => l,
            _ => Lanes::Uniform(Scalar::Num(0.0)),
        };
        let one_arg = args.len() == 2 || matches!(args.get(2), Some(Expr::Missing));
        let (rl, cl) = (as_lanes(rv), as_lanes(cv));
        let (rows, cols) = match &src {
            Value::R(r) => Self::ref_shape(r),
            Value::A(a) => (a.rows, a.cols),
            Value::S(l) if l.any_err() => return Ok(src),
            Value::S(_) => (1, 1),
            Value::LR(_) => return unsupported("INDEX of a case-dependent reference"),
        };
        // Resolve (row, col) per lane; 0 means the whole row/column.
        let resolve = |rs: &Scalar, cs: &Scalar| -> Result<(usize, usize), XlError> {
            let (mut r, mut c) = (rs.to_num()?.trunc(), cs.to_num()?.trunc());
            if one_arg && rows == 1 && cols > 1 {
                c = r;
                r = 0.0;
                if c == 0.0 {
                    r = 1.0;
                }
            }
            if r < 0.0 || c < 0.0 || r as usize > rows || c as usize > cols {
                return Err(XlError::Ref);
            }
            Ok((r as usize, c as usize))
        };
        let uniform = rl.is_uniform() && cl.is_uniform();
        let lanes = if uniform { 1 } else { self.n };
        let picks: Vec<Result<(usize, usize), XlError>> = (0..lanes).map(|i| rl.with(i, |r| cl.with(i, |c| resolve(r, c)))).collect();
        match src {
            Value::R(r) => {
                let sub = |p: &Result<(usize, usize), XlError>| -> Result<RefRange, XlError> {
                    let (pr, pc) = (*p)?;
                    let rect = Rect {
                        r1: if pr == 0 { r.rect.r1 } else { r.rect.r1 + pr as u32 - 1 },
                        r2: if pr == 0 { r.rect.r2 } else { r.rect.r1 + pr as u32 - 1 },
                        c1: if pc == 0 { r.rect.c1 } else { r.rect.c1 + pc as u32 - 1 },
                        c2: if pc == 0 { r.rect.c2 } else { r.rect.c1 + pc as u32 - 1 },
                    };
                    Ok(RefRange { sheet: r.sheet, rect })
                };
                let refs: Vec<Result<RefRange, XlError>> = picks.iter().map(sub).collect();
                if uniform {
                    return Ok(match refs[0] {
                        Ok(r) => Value::R(r),
                        Err(e) => Value::err(e),
                    });
                }
                Ok(self.lane_refs(refs))
            }
            Value::A(a) => {
                if uniform {
                    return Ok(match picks[0] {
                        Err(e) => Value::err(e),
                        Ok((0, 0)) => Value::A(a),
                        Ok((0, c)) => Value::A(Arc::new(Array::new(a.rows, 1, (0..a.rows).map(|r| a.get(r, c - 1).clone()).collect()))),
                        Ok((r, 0)) => Value::A(Arc::new(Array::new(1, a.cols, (0..a.cols).map(|c| a.get(r - 1, c).clone()).collect()))),
                        Ok((r, c)) => Value::S(a.get(r - 1, c - 1).clone()),
                    });
                }
                if picks.iter().any(|p| matches!(p, Ok((0, _)) | Ok((_, 0)))) {
                    return unsupported("INDEX returning case-dependent rows/columns of an array");
                }
                Ok(Value::S(Lanes::from_vec(
                    picks.iter().enumerate().map(|(i, p)| match p {
                        Ok((r, c)) => a.get(r - 1, c - 1).get(i),
                        Err(e) => Scalar::Err(*e),
                    })
                    .collect(),
                )))
            }
            Value::S(l) => Ok(Value::S(Lanes::from_vec(
                (0..self.n).map(|i| match picks[if uniform { 0 } else { i }] {
                    Ok(_) => l.get(i),
                    Err(e) => Scalar::Err(e),
                })
                .collect(),
            ))),
            Value::LR(_) => unreachable!(),
        }
    }

    /// INDEX with array-valued row/column arguments: element-wise gather.
    fn index_array_args(&mut self, src: Value, rv: Option<Value>, cv: Option<Value>, cx: &Ctx) -> R<Value> {
        let arr = self.to_array(src)?;
        let rv = rv.unwrap_or(Value::num(1.0));
        let cv = cv.unwrap_or(Value::num(if arr.cols == 1 { 1.0 } else { 0.0 }));
        let n = self.n;
        let arr2 = arr.clone();
        self.lift(vec![rv, cv], cx, &move |l| {
            crate::funcs::lanewise(n, l, &|s| {
                let (r, c) = match (s[0].to_num(), s[1].to_num()) {
                    (Ok(r), Ok(c)) => (r.trunc() as isize, c.trunc() as isize),
                    (Err(e), _) | (_, Err(e)) => return Scalar::Err(e),
                };
                let (r, c) = if arr2.rows == 1 && c == 0 { (1, r) } else { (r, c.max(1)) };
                if r < 1 || c < 1 || r as usize > arr2.rows || c as usize > arr2.cols {
                    return Scalar::Err(XlError::Ref);
                }
                // Note: element lanes are uniform here only; mixed sources read lane 0.
                arr2.get(r as usize - 1, c as usize - 1).get(0)
            })
        })
        .and_then(|v| {
            if arr.all_uniform() { Ok(v) } else { unsupported("INDEX with array arguments over case-dependent data") }
        })
    }

    // ------------------------------------------------------------------ MATCH & co

    /// 1-D elements of a lookup vector and a cache key when it comes from a reference.
    fn vector(&mut self, v: Value) -> R<(Arc<Array>, Option<RefRange>)> {
        let key = if let Value::R(r) = &v { Some(*r) } else { None };
        let a = self.to_array(v)?;
        if a.rows != 1 && a.cols != 1 {
            return Err(crate::eval::EvalError::Other("lookup vector must be one row or column".into()));
        }
        Ok((a, key))
    }

    pub(crate) fn positions(&mut self, needle: &Lanes, vec: &Arc<Array>, key: Option<RefRange>, mode: Mode) -> Vec<Pos> {
        let n = self.n;
        let lanes = if needle.is_uniform() && vec.all_uniform() { 1 } else { n };
        if vec.all_uniform() {
            let tag = match mode {
                Mode::Exact { .. } => 0,
                _ => 1,
            };
            let idx = match key.and_then(|k| self.lookup_cache.get(&(k, tag)).cloned()) {
                Some(i) => i,
                None => {
                    let values: Vec<Scalar> = vec.data.iter().map(|l| l.as_uniform().unwrap().clone()).collect();
                    let i = Arc::new(if tag == 0 { Index::build(values) } else { Index { values, ..Default::default() } });
                    if let Some(k) = key {
                        self.lookup_cache.insert((k, tag), i.clone());
                    }
                    i
                }
            };
            let mut memo: HashMap<String, Pos> = HashMap::new();
            return (0..lanes)
                .map(|i| {
                    needle.with(i, |s| {
                        if let Scalar::Err(e) = s {
                            return Err(*e);
                        }
                        match mode {
                            Mode::Exact { wild, last } => {
                                if let (true, Scalar::Text(t)) = (wild, s) {
                                    if has_wildcard(t) {
                                        if let Some(p) = memo.get(&**t) {
                                            return *p;
                                        }
                                        let mut it: Box<dyn Iterator<Item = (usize, &Scalar)>> =
                                            if last { Box::new(idx.values.iter().enumerate().rev()) } else { Box::new(idx.values.iter().enumerate()) };
                                        let p = it.find(|(_, v)| lookup_eq(s, v, true)).map(|(k, _)| k).ok_or(XlError::NA);
                                        memo.insert(t.to_string(), p);
                                        return p;
                                    }
                                }
                                idx.exact(s, last).ok_or(XlError::NA)
                            }
                            m => search(s, &idx.values, m),
                        }
                    })
                })
                .collect();
        }
        (0..n)
            .map(|i| {
                let vals: Vec<Scalar> = vec.data.iter().map(|l| l.get(i)).collect();
                needle.with(i, |s| {
                    if let Scalar::Err(e) = s {
                        return Err(*e);
                    }
                    match mode {
                        Mode::Exact { wild, last } => {
                            let w = wild && matches!(s, Scalar::Text(t) if has_wildcard(t));
                            if last {
                                vals.iter().rposition(|v| lookup_eq(s, v, w)).ok_or(XlError::NA)
                            } else {
                                vals.iter().position(|v| lookup_eq(s, v, w)).ok_or(XlError::NA)
                            }
                        }
                        m => search(s, &vals, m),
                    }
                })
            })
            .collect()
    }

    fn f_match(&mut self, args: &[Expr], cx: &Ctx, x: bool) -> R<Value> {
        if args.len() < 2 || args.len() > if x { 4 } else { 3 } {
            return Ok(Value::err(XlError::Value));
        }
        let needle = self.eval(&args[0], cx)?;
        let needle = self.operand(needle, cx)?;
        let v = self.eval(&args[1], cx)?;
        let (vec, key) = match self.vector(v) {
            Ok(x) => x,
            Err(_) => return Ok(Value::err(XlError::NA)),
        };
        let mt = match args.get(2) {
            None | Some(Expr::Missing) => if x { 0.0 } else { 1.0 },
            Some(e) => match self.uniform_num(e, cx)? {
                Ok(v) => v,
                Err(e) => return Ok(Value::err(e)),
            },
        };
        let sm = match args.get(3) {
            None | Some(Expr::Missing) => 1.0,
            Some(e) => match self.uniform_num(e, cx)? {
                Ok(v) => v,
                Err(e) => return Ok(Value::err(e)),
            },
        };
        let mode = if x {
            match (mt as i32, sm as i32) {
                (0, 1) => Mode::Exact { wild: false, last: false },
                (0, -1) => Mode::Exact { wild: false, last: true },
                (2, s) => Mode::Exact { wild: true, last: s == -1 },
                (-1, 1 | -1) => Mode::NextSmaller,
                (1, 1 | -1) => Mode::NextLarger,
                (0, 2) | (0, -2) => Mode::Exact { wild: false, last: false },
                _ => return unsupported("XMATCH mode"),
            }
        } else if mt > 0.0 {
            Mode::SortedAsc
        } else if mt < 0.0 {
            Mode::SortedDesc
        } else {
            Mode::Exact { wild: true, last: false }
        };
        let n = self.n;
        let needles = match needle {
            Value::S(l) => vec![l],
            Value::A(a) => a.data.clone(),
            _ => unreachable!(),
        };
        let mut outs = Vec::new();
        for nd in &needles {
            let pos = self.positions(nd, &vec, key, mode);
            outs.push(if pos.len() == 1 {
                Lanes::Uniform(match pos[0] {
                    Ok(p) => Scalar::Num((p + 1) as f64),
                    Err(e) => Scalar::Err(e),
                })
            } else {
                Lanes::from_vec((0..n).map(|i| match pos[i] {
                    Ok(p) => Scalar::Num((p + 1) as f64),
                    Err(e) => Scalar::Err(e),
                }).collect())
            });
        }
        Ok(if outs.len() == 1 && !matches!(args[0], Expr::Array(_)) && needles.len() == 1 {
            Value::S(outs.pop().unwrap())
        } else {
            let (rows, cols) = match self.eval(&args[0], &Ctx { array: true, ..*cx })? {
                Value::A(a) => (a.rows, a.cols),
                Value::R(r) => Self::ref_shape(&r),
                _ => (1, 1),
            };
            if rows * cols != outs.len() {
                Value::S(outs[0].clone())
            } else {
                Value::A(Arc::new(Array::new(rows, cols, outs)))
            }
        })
    }

    pub(crate) fn uniform_num(&mut self, e: &Expr, cx: &Ctx) -> R<Result<f64, XlError>> {
        let v = self.eval(e, cx)?;
        let l = self.to_lanes(v, cx)?;
        match l.as_uniform() {
            Some(s) => Ok(s.to_num()),
            None => unsupported("case-dependent mode argument"),
        }
    }

    /// Pick the row (vertical) or column of `ret` at each lane position.
    fn take_at(&mut self, ret: Value, vertical: bool, pos: &[Pos], not_found: Option<&Lanes>) -> R<Value> {
        let n = self.n;
        let pick_ref = |r: &RefRange, p: usize| -> RefRange {
            let rect = if vertical {
                Rect { r1: r.rect.r1 + p as u32, r2: r.rect.r1 + p as u32, ..r.rect }
            } else {
                Rect { c1: r.rect.c1 + p as u32, c2: r.rect.c1 + p as u32, ..r.rect }
            };
            RefRange { sheet: r.sheet, rect }
        };
        if not_found.is_none() || pos.iter().all(|p| p.is_ok()) {
            if let Value::R(r) = &ret {
                let refs: Vec<Result<RefRange, XlError>> = pos.iter().map(|p| p.map(|p| pick_ref(r, p))).collect();
                if refs.len() == 1 {
                    return Ok(match refs[0] {
                        Ok(r) => Value::R(r),
                        Err(e) => Value::err(e),
                    });
                }
                return Ok(self.lane_refs(refs));
            }
        }
        let arr = self.to_array(ret)?;
        let width = if vertical { arr.cols } else { arr.rows };
        let mut data = Vec::with_capacity(width);
        for k in 0..width {
            let lanes: Vec<Scalar> = (0..if pos.len() == 1 && not_found.is_none_or(Lanes::is_uniform) && arr.all_uniform() { 1 } else { n })
                .map(|i| match pos[if pos.len() == 1 { 0 } else { i }] {
                    Ok(p) => {
                        let (r, c) = if vertical { (p, k) } else { (k, p) };
                        if r < arr.rows && c < arr.cols { arr.get(r, c).get(i) } else { Scalar::Err(XlError::NA) }
                    }
                    Err(XlError::NA) if not_found.is_some() => not_found.unwrap().get(i),
                    Err(e) => Scalar::Err(e),
                })
                .collect();
            data.push(if lanes.len() == 1 { Lanes::Uniform(lanes.into_iter().next().unwrap()) } else { Lanes::from_vec(lanes) });
        }
        if width == 1 {
            return Ok(Value::S(data.pop().unwrap()));
        }
        let (rows, cols) = if vertical { (1, width) } else { (width, 1) };
        Ok(Value::A(Arc::new(Array::new(rows, cols, data))))
    }

    fn f_xlookup(&mut self, args: &[Expr], cx: &Ctx) -> R<Value> {
        if args.len() < 3 || args.len() > 6 {
            return Ok(Value::err(XlError::Value));
        }
        let needle = self.eval(&args[0], cx)?;
        let needle = self.to_lanes(needle, cx)?;
        let lv = self.eval(&args[1], cx)?;
        let vertical = match &lv {
            Value::R(r) => r.rect.cols() == 1,
            Value::A(a) => a.cols == 1,
            _ => true,
        };
        let (vec, key) = self.vector(lv)?;
        let ret = self.eval(&args[2], cx)?;
        let not_found = match args.get(3) {
            None | Some(Expr::Missing) => None,
            Some(e) => {
                let v = self.eval(e, cx)?;
                Some(self.to_lanes(v, cx)?)
            }
        };
        let mm = match args.get(4) {
            None | Some(Expr::Missing) => 0.0,
            Some(e) => self.uniform_num(e, cx)?.unwrap_or(0.0),
        };
        let sm = match args.get(5) {
            None | Some(Expr::Missing) => 1.0,
            Some(e) => self.uniform_num(e, cx)?.unwrap_or(1.0),
        };
        let mode = match mm as i32 {
            0 => Mode::Exact { wild: false, last: sm < 0.0 },
            2 => Mode::Exact { wild: true, last: sm < 0.0 },
            -1 => Mode::NextSmaller,
            1 => Mode::NextLarger,
            _ => return Ok(Value::err(XlError::Value)),
        };
        let pos = self.positions(&needle, &vec, key, mode);
        self.take_at(ret, vertical, &pos, not_found.as_ref())
    }

    fn f_vhlookup(&mut self, vertical: bool, args: &[Expr], cx: &Ctx) -> R<Value> {
        if args.len() < 3 || args.len() > 4 {
            return Ok(Value::err(XlError::Value));
        }
        let needle = self.eval(&args[0], cx)?;
        let needle = self.to_lanes(needle, cx)?;
        let table = self.eval(&args[1], cx)?;
        let k = match self.uniform_num(&args[2], cx)? {
            Ok(k) => k.trunc() as usize,
            Err(e) => return Ok(Value::err(e)),
        };
        let approx_mode = match args.get(3) {
            None | Some(Expr::Missing) => true,
            Some(e) => {
                let v = self.eval(e, cx)?;
                match self.to_lanes(v, cx)?.as_uniform() {
                    Some(s) => s.to_bool().unwrap_or(true),
                    None => return unsupported("case-dependent range_lookup"),
                }
            }
        };
        let (key_vec, ret, keyref) = match &table {
            Value::R(r) => {
                let (rows, cols) = Self::ref_shape(r);
                if k < 1 || k > if vertical { cols } else { rows } {
                    return Ok(Value::err(if k < 1 { XlError::Value } else { XlError::Ref }));
                }
                let first = if vertical { Rect { c2: r.rect.c1, ..r.rect } } else { Rect { r2: r.rect.r1, ..r.rect } };
                let col = if vertical {
                    Rect { c1: r.rect.c1 + k as u32 - 1, c2: r.rect.c1 + k as u32 - 1, ..r.rect }
                } else {
                    Rect { r1: r.rect.r1 + k as u32 - 1, r2: r.rect.r1 + k as u32 - 1, ..r.rect }
                };
                let fr = RefRange { sheet: r.sheet, rect: first };
                (self.deref(&fr)?, Value::R(RefRange { sheet: r.sheet, rect: col }), Some(fr))
            }
            _ => {
                let a = self.to_array(table)?;
                if k < 1 || k > if vertical { a.cols } else { a.rows } {
                    return Ok(Value::err(XlError::Ref));
                }
                let first: Vec<Lanes> = if vertical { (0..a.rows).map(|r| a.get(r, 0).clone()).collect() } else { a.data[..a.cols].to_vec() };
                let ret: Vec<Lanes> = if vertical { (0..a.rows).map(|r| a.get(r, k - 1).clone()).collect() } else { (0..a.cols).map(|c| a.get(k - 1, c).clone()).collect() };
                let len = first.len();
                let shape = |d: Vec<Lanes>| if vertical { Array::new(len, 1, d) } else { Array::new(1, len, d) };
                (Arc::new(shape(first)), Value::A(Arc::new(shape(ret))), None)
            }
        };
        let mode = if approx_mode { Mode::SortedAsc } else { Mode::Exact { wild: true, last: false } };
        let pos = self.positions(&needle, &key_vec, keyref, mode);
        let v = self.take_at(ret, vertical, &pos, None)?;
        Ok(match v {
            Value::R(_) | Value::LR(_) => Value::S(self.to_lanes(v, cx)?),
            v => v,
        })
    }

    fn f_lookup(&mut self, args: &[Expr], cx: &Ctx) -> R<Value> {
        if args.len() != 3 {
            return unsupported("LOOKUP array form");
        }
        let needle = self.eval(&args[0], cx)?;
        let needle = self.to_lanes(needle, cx)?;
        let lv = self.eval(&args[1], cx)?;
        let vertical = match &lv {
            Value::R(r) => r.rect.cols() == 1,
            Value::A(a) => a.cols == 1,
            _ => true,
        };
        let (vec, key) = self.vector(lv)?;
        let ret = self.eval(&args[2], cx)?;
        let pos = self.positions(&needle, &vec, key, Mode::SortedAsc);
        let rv = match &ret {
            Value::R(r) => r.rect.cols() == 1,
            Value::A(a) => a.cols == 1,
            _ => vertical,
        };
        let v = self.take_at(ret, rv, &pos, None)?;
        Ok(match v {
            Value::R(_) | Value::LR(_) => Value::S(self.to_lanes(v, cx)?),
            v => v,
        })
    }

    // ------------------------------------------------------------------ OFFSET/INDIRECT/ADDRESS

    fn f_offset(&mut self, args: &[Expr], cx: &Ctx) -> R<Value> {
        if args.len() < 3 || args.len() > 5 {
            return Ok(Value::err(XlError::Value));
        }
        let base = self.eval(&args[0], cx)?;
        let mut nums = Vec::new();
        for a in &args[1..] {
            nums.push(match a {
                Expr::Missing => None,
                e => {
                    let v = self.eval(e, cx)?;
                    Some(self.to_lanes(v, cx)?)
                }
            });
        }
        while nums.len() < 4 {
            nums.push(None);
        }
        let bases: Vec<Result<RefRange, XlError>> = match &base {
            Value::R(r) => vec![Ok(*r)],
            Value::LR(v) => v.as_ref().clone(),
            Value::S(l) if l.any_err() => return Ok(base),
            _ => return Ok(Value::err(XlError::Value)),
        };
        let uniform = bases.len() == 1 && nums.iter().all(|x| x.as_ref().is_none_or(Lanes::is_uniform));
        let lanes = if uniform { 1 } else { self.n };
        let get = |l: &Option<Lanes>, i: usize, d: f64| -> Result<f64, XlError> {
            match l {
                None => Ok(d),
                Some(l) => l.with(i, |s| s.to_num()).map(f64::trunc),
            }
        };
        let refs: Vec<Result<RefRange, XlError>> = (0..lanes)
            .map(|i| {
                let b = bases[if bases.len() == 1 { 0 } else { i }]?;
                let (h0, w0) = (b.rect.rows() as f64, b.rect.cols() as f64);
                let (dr, dc) = (get(&nums[0], i, 0.0)?, get(&nums[1], i, 0.0)?);
                let (h, w) = (get(&nums[2], i, h0)?, get(&nums[3], i, w0)?);
                if h == 0.0 || w == 0.0 {
                    return Err(XlError::Ref);
                }
                let r1 = b.rect.r1 as f64 + dr;
                let c1 = b.rect.c1 as f64 + dc;
                let (r1, r2) = if h > 0.0 { (r1, r1 + h - 1.0) } else { (r1 + h + 1.0, r1) };
                let (c1, c2) = if w > 0.0 { (c1, c1 + w - 1.0) } else { (c1 + w + 1.0, c1) };
                if r1 < 1.0 || c1 < 1.0 || r2 > 1_048_576.0 || c2 > 16_384.0 {
                    return Err(XlError::Ref);
                }
                Ok(RefRange { sheet: b.sheet, rect: Rect { r1: r1 as u32, c1: c1 as u32, r2: r2 as u32, c2: c2 as u32 } })
            })
            .collect();
        Ok(if uniform {
            match refs[0] {
                Ok(r) => Value::R(r),
                Err(e) => Value::err(e),
            }
        } else {
            self.lane_refs(refs)
        })
    }

    fn f_indirect(&mut self, args: &[Expr], cx: &Ctx) -> R<Value> {
        if args.is_empty() || args.len() > 2 {
            return Ok(Value::err(XlError::Value));
        }
        if let Some(e) = args.get(1) {
            if !matches!(e, Expr::Missing) && !matches!(self.uniform_num(e, cx)?, Ok(v) if v != 0.0) {
                return unsupported("INDIRECT with R1C1 references");
            }
        }
        let t = self.eval(&args[0], cx)?;
        let t = self.to_lanes(t, cx)?;
        let mut cache: HashMap<String, Result<RefRange, XlError>> = HashMap::new();
        let lanes = if t.is_uniform() { 1 } else { self.n };
        let mut refs = Vec::with_capacity(lanes);
        for i in 0..lanes {
            let s = t.get(i);
            let r = match s.to_text() {
                Err(e) => Err(e),
                Ok(text) => {
                    if let Some(r) = cache.get(&*text) {
                        *r
                    } else {
                        let r = match xlformula::parse(&text, cx.cell) {
                            Ok(Expr::Ref(rf)) => match self.resolve_ref(&rf, &Ctx { cell: Cell { row: 1, col: 1 }, ..*cx }) {
                                Value::R(r) => Ok(r),
                                _ => Err(XlError::Ref),
                            },
                            Ok(e @ Expr::Name(_)) => match self.eval(&e, cx)? {
                                Value::R(r) => Ok(r),
                                _ => Err(XlError::Ref),
                            },
                            Ok(e @ Expr::Binary(xlformula::BinOp::Range, ..)) => match self.eval(&e, cx)? {
                                Value::R(r) => Ok(r),
                                _ => Err(XlError::Ref),
                            },
                            _ => Err(XlError::Ref),
                        };
                        cache.insert(text.to_string(), r);
                        r
                    }
                }
            };
            refs.push(r);
        }
        Ok(if lanes == 1 {
            match refs[0] {
                Ok(r) => Value::R(r),
                Err(e) => Value::err(e),
            }
        } else {
            self.lane_refs(refs)
        })
    }

    fn f_address(&mut self, args: &[Expr], cx: &Ctx) -> R<Value> {
        if args.len() < 2 || args.len() > 5 {
            return Ok(Value::err(XlError::Value));
        }
        let mut vals = Vec::new();
        for (i, a) in args.iter().enumerate() {
            vals.push(match a {
                Expr::Missing => Value::S(Lanes::Uniform(match i {
                    2 => Scalar::Num(1.0),
                    3 => Scalar::Bool(true),
                    _ => Scalar::Empty,
                })),
                e => self.eval(e, cx)?,
            });
        }
        while vals.len() < 4 {
            vals.push(Value::S(Lanes::Uniform(if vals.len() == 2 { Scalar::Num(1.0) } else { Scalar::Bool(true) })));
        }
        let has_sheet = vals.len() == 5;
        let n = self.n;
        self.lift(vals, cx, &move |l| {
            crate::funcs::lanewise(n, l, &|s| {
                if let Some(Scalar::Err(e)) = s.iter().find(|x| x.is_err()) {
                    return Scalar::Err(*e);
                }
                let (Ok(r), Ok(c), Ok(abs), Ok(a1)) = (s[0].to_num(), s[1].to_num(), s[2].to_num(), s[3].to_bool()) else {
                    return Scalar::Err(XlError::Value);
                };
                let (r, c, abs) = (r.trunc() as i64, c.trunc() as i64, abs.trunc() as i64);
                if r < 1 || c < 1 || r > 1_048_576 || c > 16_384 || !(1..=4).contains(&abs) {
                    return Scalar::Err(XlError::Value);
                }
                let (ra, ca) = (abs == 1 || abs == 2, abs == 1 || abs == 3);
                let body = if a1 {
                    format!("{}{}{}{}", if ca { "$" } else { "" }, xlmodel::col_name(c as u32), if ra { "$" } else { "" }, r)
                } else {
                    format!("R{}C{}", if ra { r.to_string() } else { format!("[{r}]") }, if ca { c.to_string() } else { format!("[{c}]") })
                };
                if has_sheet {
                    if let Ok(sh) = s[4].to_text() {
                        if !sh.is_empty() {
                            let quote = sh.chars().any(|ch| !(ch.is_alphanumeric() || ch == '_' || ch == '.'));
                            return Scalar::text(if quote { format!("'{}'!{body}", sh.replace('\'', "''")) } else { format!("{sh}!{body}") });
                        }
                    }
                }
                Scalar::text(body)
            })
        })
    }

    // ------------------------------------------------------------------ COUNTIF family

    fn f_ifs_agg(&mut self, name: &str, args: &[Expr], cx: &Ctx) -> R<Value> {
        // Normalise to (target range or None, [(range, criterion)]).
        let (target_expr, pairs): (Option<&Expr>, Vec<(&Expr, &Expr)>) = match name {
            "COUNTIF" => {
                if args.len() != 2 {
                    return Ok(Value::err(XlError::Value));
                }
                (None, vec![(&args[0], &args[1])])
            }
            "SUMIF" | "AVERAGEIF" => {
                if args.len() < 2 || args.len() > 3 {
                    return Ok(Value::err(XlError::Value));
                }
                (Some(args.get(2).filter(|e| !matches!(e, Expr::Missing)).unwrap_or(&args[0])), vec![(&args[0], &args[1])])
            }
            "COUNTIFS" => {
                if args.len() < 2 || args.len() % 2 == 1 {
                    return Ok(Value::err(XlError::Value));
                }
                (None, args.chunks(2).map(|p| (&p[0], &p[1])).collect())
            }
            _ => {
                if args.len() < 3 || args.len() % 2 == 0 {
                    return Ok(Value::err(XlError::Value));
                }
                (Some(&args[0]), args[1..].chunks(2).map(|p| (&p[0], &p[1])).collect())
            }
        };
        let mut ranges = Vec::new();
        let mut first_ref: Option<RefRange> = None;
        for (re, _) in &pairs {
            let v = self.eval(re, cx)?;
            if first_ref.is_none() {
                if let Value::R(r) = &v {
                    first_ref = Some(*r);
                }
            }
            ranges.push(self.to_array(v)?);
        }
        let target = match target_expr {
            None => None,
            Some(e) => {
                let mut v = self.eval(e, cx)?;
                // SUMIF/AVERAGEIF: sum_range takes the shape of range from its top-left.
                if matches!(name, "SUMIF" | "AVERAGEIF") {
                    if let (Value::R(t), Some(r0)) = (&v, first_ref) {
                        v = Value::R(RefRange {
                            sheet: t.sheet,
                            rect: Rect { r2: t.rect.r1 + r0.rect.rows() - 1, c2: t.rect.c1 + r0.rect.cols() - 1, ..t.rect },
                        });
                    }
                }
                Some(self.to_array(v)?)
            }
        };
        let len = ranges[0].data.len();
        if ranges.iter().any(|r| r.data.len() != len) || target.as_ref().is_some_and(|t| t.data.len() != len) {
            // Clamped whole-column ranges may differ in materialised size: pad by Empty.
            let max = ranges.iter().map(|r| r.data.len()).chain(target.iter().map(|t| t.data.len())).max().unwrap();
            let pad = |a: &Arc<Array>| -> Arc<Array> {
                if a.data.len() == max || a.cols != 1 {
                    return a.clone();
                }
                let mut d = a.data.clone();
                d.resize(max, Lanes::Uniform(Scalar::Empty));
                Arc::new(Array::new(max, 1, d))
            };
            ranges = ranges.iter().map(pad).collect();
            if ranges.iter().any(|r| r.data.len() != max) {
                return Ok(Value::err(XlError::Value));
            }
            return self.ifs_finish(name, &pairs, ranges, target.map(|t| pad(&t)), cx);
        }
        self.ifs_finish(name, &pairs, ranges, target, cx)
    }

    fn ifs_finish(&mut self, name: &str, pairs: &[(&Expr, &Expr)], ranges: Vec<Arc<Array>>, target: Option<Arc<Array>>, cx: &Ctx) -> R<Value> {
        let mut crits = Vec::new();
        for (_, ce) in pairs {
            let v = self.eval(ce, cx)?;
            let v = self.operand(v, cx)?;
            match v {
                Value::S(l) => crits.push(l),
                Value::A(_) => return unsupported(format!("{name} with array criteria")),
                _ => unreachable!(),
            }
        }
        let n = self.n;
        let uniform = crits.iter().all(Lanes::is_uniform) && ranges.iter().all(|r| r.all_uniform()) && target.as_ref().is_none_or(|t| t.all_uniform());
        let data_uniform = ranges.iter().all(|r| r.all_uniform()) && target.as_ref().is_none_or(|t| t.all_uniform());
        let len = ranges[0].data.len();
        // Pre-extract uniform data once.
        let uvals: Option<Vec<Vec<Scalar>>> =
            data_uniform.then(|| ranges.iter().map(|r| r.data.iter().map(|l| l.as_uniform().unwrap().clone()).collect()).collect());
        let utarget: Option<Vec<Scalar>> = data_uniform.then(|| target.as_ref().map(|t| t.data.iter().map(|l| l.as_uniform().unwrap().clone()).collect())).flatten();
        let mut crit_cache: HashMap<Vec<String>, Scalar> = HashMap::new();
        let lane = |i: usize, crit_cache: &mut HashMap<Vec<String>, Scalar>| -> Scalar {
            let cs: Vec<Scalar> = crits.iter().map(|c| c.get(i)).collect();
            if let Some(Scalar::Err(e)) = cs.iter().find(|c| c.is_err()) {
                return Scalar::Err(*e);
            }
            let key: Option<Vec<String>> = data_uniform.then(|| cs.iter().map(|c| format!("{c:?}")).collect());
            if let Some(k) = &key {
                if let Some(v) = crit_cache.get(k) {
                    return v.clone();
                }
            }
            let parsed: Vec<Criterion> = cs.iter().map(Criterion::parse).collect();
            let (mut count, mut sum) = (0usize, 0.0f64);
            let (mut min, mut max) = (f64::INFINITY, f64::NEG_INFINITY);
            for k in 0..len {
                let hit = parsed.iter().enumerate().all(|(j, c)| match &uvals {
                    Some(u) => c.test(&u[j][k]),
                    None => c.test(&ranges[j].data[k].get(i)),
                });
                if !hit {
                    continue;
                }
                match (&utarget, &target) {
                    (Some(t), _) => match &t[k] {
                        Scalar::Num(x) => {
                            count += 1;
                            sum += x;
                            min = min.min(*x);
                            max = max.max(*x);
                        }
                        Scalar::Err(e) => return Scalar::Err(*e),
                        _ => {}
                    },
                    (None, Some(t)) => match t.data[k].get(i) {
                        Scalar::Num(x) => {
                            count += 1;
                            sum += x;
                            min = min.min(x);
                            max = max.max(x);
                        }
                        Scalar::Err(e) => return Scalar::Err(e),
                        _ => {}
                    },
                    (None, None) => count += 1,
                }
            }
            let r = match name {
                "COUNTIF" | "COUNTIFS" => Scalar::Num(count as f64),
                "SUMIF" | "SUMIFS" => Scalar::num(sum),
                "AVERAGEIF" | "AVERAGEIFS" => {
                    if count == 0 { Scalar::Err(XlError::Div0) } else { Scalar::num(sum / count as f64) }
                }
                "MINIFS" => Scalar::num(if count == 0 { 0.0 } else { min }),
                _ => Scalar::num(if count == 0 { 0.0 } else { max }),
            };
            if let Some(k) = key {
                crit_cache.insert(k, r.clone());
            }
            r
        };
        if uniform {
            return Ok(Value::S(Lanes::Uniform(lane(0, &mut crit_cache))));
        }
        Ok(Value::S(Lanes::from_vec((0..n).map(|i| lane(i, &mut crit_cache)).collect())))
    }
}

/// Approximate / next-value search over one lane's values.
fn search(needle: &Scalar, vals: &[Scalar], mode: Mode) -> Pos {
    match mode {
        Mode::SortedAsc => approx(needle, vals, false).ok_or(XlError::NA),
        Mode::SortedDesc => approx(needle, vals, true).ok_or(XlError::NA),
        Mode::NextSmaller | Mode::NextLarger => {
            let mut best: Option<usize> = None;
            for (i, v) in vals.iter().enumerate() {
                let same = std::mem::discriminant(v) == std::mem::discriminant(needle);
                if !same {
                    continue;
                }
                match compare(v, needle) {
                    Ordering::Equal => return Ok(i),
                    o => {
                        let ok = (mode == Mode::NextSmaller && o == Ordering::Less) || (mode == Mode::NextLarger && o == Ordering::Greater);
                        if ok {
                            let better = match best {
                                None => true,
                                Some(b) => {
                                    let c = compare(v, &vals[b]);
                                    if mode == Mode::NextSmaller { c == Ordering::Greater } else { c == Ordering::Less }
                                }
                            };
                            if better {
                                best = Some(i);
                            }
                        }
                    }
                }
            }
            best.ok_or(XlError::NA)
        }
        Mode::Exact { .. } => unreachable!(),
    }
}
