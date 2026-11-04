//! Function dispatch, control flow, logical and aggregate functions.
use crate::eval::{unsupported, Ctx, Job, R};
use crate::lanes::{select, Lanes};
use crate::scalar::Scalar;
use crate::scalars::{scalar_fn, ScalarFn};
use crate::value::{Array, Value};
use std::sync::Arc;
use xlformula::Expr;
use xlmodel::XlError;

pub(crate) fn lanewise(n: usize, ls: &[&Lanes], f: &dyn Fn(&[Scalar]) -> Scalar) -> Lanes {
    if ls.iter().all(|l| l.is_uniform()) {
        let s: Vec<Scalar> = ls.iter().map(|l| l.as_uniform().unwrap().clone()).collect();
        return Lanes::Uniform(f(&s));
    }
    let mut out = Vec::with_capacity(n);
    let mut buf = Vec::with_capacity(ls.len());
    for i in 0..n {
        buf.clear();
        buf.extend(ls.iter().map(|l| l.get(i)));
        out.push(f(&buf));
    }
    Lanes::from_vec(out)
}

/// How an aggregate treats one argument's elements.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum ArgKind {
    /// Values from references/arrays: text and logicals are skipped.
    Range,
    /// A directly supplied scalar: text is coerced, logicals count.
    Direct,
}

impl<'m> Job<'m> {
    pub fn call(&mut self, name: &str, args: &[Expr], cx: &Ctx) -> R<Value> {
        let n = self.n;
        match name {
            "IF" => self.f_if(args, cx),
            "IFS" => self.f_ifs(args, cx),
            "IFERROR" => self.f_iferror(args, cx, false),
            "IFNA" => self.f_iferror(args, cx, true),
            "CHOOSE" => self.f_choose(args, cx),
            "SWITCH" => self.f_switch(args, cx),
            "LET" => self.f_let(args, cx),
            "AND" | "OR" | "XOR" => self.f_logical(name, args, cx),
            "SUM" | "MIN" | "MAX" | "AVERAGE" | "COUNT" | "COUNTA" | "PRODUCT" | "SUMSQ" | "MEDIAN" | "COUNTBLANK" => {
                self.f_aggregate(name, args, cx)
            }
            "SUMPRODUCT" => self.f_sumproduct(args, cx),
            "CONCAT" | "TEXTJOIN" => self.f_concat(name, args, cx),
            "TRUE" => Ok(Value::S(Lanes::Uniform(Scalar::Bool(true)))),
            "FALSE" => Ok(Value::S(Lanes::Uniform(Scalar::Bool(false)))),
            "PI" => Ok(Value::num(std::f64::consts::PI)),
            "NA" => Ok(Value::err(XlError::NA)),
            "_BC_UNPARSABLE" => unsupported("formula could not be parsed"),
            "_BC_DATATABLE" => unsupported("what-if data table (TABLE)"),
            _ => {
                if let Some(v) = self.call_ref(name, args, cx)? {
                    return Ok(v);
                }
                if let Some(v) = self.call_array(name, args, cx)? {
                    return Ok(v);
                }
                if let Some(v) = self.call_stat(name, args, cx)? {
                    return Ok(v);
                }
                if let Some(f) = scalar_fn(name) {
                    return self.apply_scalar(name, &f, args, cx, n);
                }
                if let Some(v) = self.call_udf(name, args, cx)? {
                    return Ok(v);
                }
                unsupported(format!("function {name}"))
            }
        }
    }

    fn apply_scalar(&mut self, name: &str, f: &ScalarFn, args: &[Expr], cx: &Ctx, n: usize) -> R<Value> {
        if args.len() < f.min || args.len() > f.max {
            return Ok(Value::err(XlError::Value));
        }
        let mut vals = Vec::with_capacity(f.max);
        for (i, a) in args.iter().enumerate() {
            vals.push(match a {
                Expr::Missing => Value::S(Lanes::Uniform(f.default(i).unwrap_or(Scalar::Empty))),
                e => self.eval(e, cx)?,
            });
        }
        for i in args.len()..f.max {
            if let Some(d) = f.default(i) {
                vals.push(Value::S(Lanes::Uniform(d)));
            }
        }
        let _ = name;
        if let (Some(num1), 1) = (f.num1, vals.len()) {
            return self.lift(vals, cx, &|l| l[0].map_num(n, num1));
        }
        let func = f.func;
        let errs = f.propagate_errors;
        self.lift(vals, cx, &|l| {
            lanewise(n, l, &|s| {
                if errs {
                    if let Some(Scalar::Err(e)) = s.iter().find(|x| x.is_err()) {
                        return Scalar::Err(*e);
                    }
                }
                func(s)
            })
        })
    }

    // ------------------------------------------------------------ control flow

    fn f_if(&mut self, args: &[Expr], cx: &Ctx) -> R<Value> {
        if args.is_empty() || args.len() > 3 {
            return Ok(Value::err(XlError::Value));
        }
        let cond = self.eval(&args[0], cx)?;
        let cond = self.operand(cond, cx)?;
        let branch = |job: &mut Self, i: usize| -> R<Value> {
            match args.get(i) {
                None => Ok(Value::S(Lanes::Uniform(Scalar::Bool(false)))),
                Some(Expr::Missing) => Ok(Value::num(0.0)),
                Some(e) => job.eval(e, cx),
            }
        };
        if let Value::S(Lanes::Uniform(c)) = &cond {
            return match c.to_bool() {
                Ok(true) => branch(self, 1),
                Ok(false) => branch(self, 2),
                Err(e) => Ok(Value::err(e)),
            };
        }
        let t = branch(self, 1)?;
        let f = branch(self, 2)?;
        let n = self.n;
        self.lift(vec![cond, t, f], cx, &|l| select(l[0], l[1], l[2], n))
    }

    fn f_ifs(&mut self, args: &[Expr], cx: &Ctx) -> R<Value> {
        if args.len() < 2 || args.len() % 2 == 1 {
            return Ok(Value::err(XlError::Value));
        }
        // Build from the last pair backwards: IFS(c1,v1,c2,v2) = IF(c1,v1,IF(c2,v2,#N/A)).
        let n = self.n;
        let mut acc = Value::err(XlError::NA);
        for pair in args.chunks(2).rev() {
            let cond = self.eval(&pair[0], cx)?;
            let cond = self.operand(cond, cx)?;
            if let Value::S(Lanes::Uniform(c)) = &cond {
                acc = match c.to_bool() {
                    Ok(true) => self.eval(&pair[1], cx)?,
                    Ok(false) => acc,
                    Err(e) => Value::err(e),
                };
                continue;
            }
            let v = self.eval(&pair[1], cx)?;
            acc = self.lift(vec![cond, v, acc], cx, &|l| select(l[0], l[1], l[2], n))?;
        }
        Ok(acc)
    }

    fn f_iferror(&mut self, args: &[Expr], cx: &Ctx, na_only: bool) -> R<Value> {
        if args.len() != 2 {
            return Ok(Value::err(XlError::Value));
        }
        let v = self.eval(&args[0], cx)?;
        let v = self.operand(v, cx)?;
        let is_hit = move |s: &Scalar| match s {
            Scalar::Err(e) => !na_only || *e == XlError::NA,
            _ => false,
        };
        let any_hit = match &v {
            Value::S(l) => (0..if l.is_uniform() { 1 } else { self.n }).any(|i| l.with(i, is_hit)),
            Value::A(a) => a.data.iter().any(|l| (0..if l.is_uniform() { 1 } else { self.n }).any(|i| l.with(i, is_hit))),
            _ => true,
        };
        if !any_hit {
            return Ok(v);
        }
        let alt = match &args[1] {
            Expr::Missing => Value::num(0.0),
            e => self.eval(e, cx)?,
        };
        let n = self.n;
        self.lift(vec![v, alt], cx, &|l| {
            if let Lanes::Uniform(s) = l[0] {
                return if is_hit(s) { l[1].clone() } else { l[0].clone() };
            }
            Lanes::from_vec((0..n).map(|i| if l[0].with(i, is_hit) { l[1].get(i) } else { l[0].get(i) }).collect())
        })
    }

    fn f_choose(&mut self, args: &[Expr], cx: &Ctx) -> R<Value> {
        if args.len() < 2 {
            return Ok(Value::err(XlError::Value));
        }
        let idx = self.eval(&args[0], cx)?;
        let idx = self.to_lanes(idx, cx)?;
        let pick = |s: &Scalar| -> Result<usize, XlError> {
            let k = s.to_num()?.trunc();
            if k < 1.0 || k as usize >= args.len() { Err(XlError::Value) } else { Ok(k as usize) }
        };
        if let Lanes::Uniform(s) = &idx {
            return match pick(s) {
                Ok(k) => self.eval(&args[k], cx),
                Err(e) => Ok(Value::err(e)),
            };
        }
        let mut options = Vec::new();
        for a in &args[1..] {
            let v = self.eval(a, cx)?;
            options.push(self.to_lanes(v, cx)?);
        }
        Ok(Value::S(Lanes::from_vec(
            (0..self.n)
                .map(|i| match idx.with(i, pick) {
                    Ok(k) => options[k - 1].get(i),
                    Err(e) => Scalar::Err(e),
                })
                .collect(),
        )))
    }

    fn f_switch(&mut self, args: &[Expr], cx: &Ctx) -> R<Value> {
        if args.len() < 3 {
            return Ok(Value::err(XlError::Value));
        }
        let v = self.eval(&args[0], cx)?;
        let v = self.to_lanes(v, cx)?;
        let has_default = args.len() % 2 == 0;
        let pairs = (args.len() - 1) / 2;
        let mut acc = if has_default {
            let d = self.eval(&args[args.len() - 1], cx)?;
            self.to_lanes(d, cx)?
        } else {
            Lanes::Uniform(Scalar::Err(XlError::NA))
        };
        let n = self.n;
        for p in (0..pairs).rev() {
            let key = self.eval(&args[1 + 2 * p], cx)?;
            let key = self.to_lanes(key, cx)?;
            let res = self.eval(&args[2 + 2 * p], cx)?;
            let res = self.to_lanes(res, cx)?;
            let eq = crate::lanes::zip2(&v, &key, n, |a, b| match (a, b) {
                (Scalar::Err(e), _) => Scalar::Err(*e),
                _ => Scalar::Bool(crate::scalar::compare(a, b) == std::cmp::Ordering::Equal && std::mem::discriminant(a) == std::mem::discriminant(b)),
            });
            acc = select(&eq, &res, &acc, n);
        }
        Ok(Value::S(acc))
    }

    fn f_let(&mut self, args: &[Expr], cx: &Ctx) -> R<Value> {
        if args.len() < 3 || args.len() % 2 == 0 {
            return Ok(Value::err(XlError::Value));
        }
        let depth = self.env.len();
        let mut result = Ok(Value::err(XlError::Value));
        let mut ok = true;
        for pair in args[..args.len() - 1].chunks(2) {
            let Expr::Name(name) = &pair[0] else {
                ok = false;
                break;
            };
            match self.eval(&pair[1], cx) {
                Ok(v) => self.env.push((name.clone(), v)),
                Err(e) => {
                    result = Err(e);
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            result = self.eval(&args[args.len() - 1], cx);
        }
        self.env.truncate(depth);
        result
    }

    // ------------------------------------------------------------ logical & aggregates

    /// Flatten arguments to (element lanes, kind) for aggregation.
    pub(crate) fn agg_items(&mut self, args: &[Expr], cx: &Ctx) -> R<Vec<(Lanes, ArgKind)>> {
        let mut items = Vec::new();
        for a in args {
            if matches!(a, Expr::Missing) {
                items.push((Lanes::Uniform(Scalar::Empty), ArgKind::Direct));
                continue;
            }
            match self.eval(a, cx)? {
                Value::S(l) => items.push((l, ArgKind::Direct)),
                v => {
                    let arr = self.to_array(v)?;
                    items.extend(arr.data.iter().cloned().map(|l| (l, ArgKind::Range)));
                }
            }
        }
        Ok(items)
    }

    fn f_logical(&mut self, name: &str, args: &[Expr], cx: &Ctx) -> R<Value> {
        let items = self.agg_items(args, cx)?;
        let f = |vals: &mut dyn Iterator<Item = (Scalar, ArgKind)>| -> Scalar {
            let (mut any, mut all, mut count, mut odd) = (false, true, 0, false);
            for (s, kind) in vals {
                let b = match (&s, kind) {
                    (Scalar::Err(e), _) => return Scalar::Err(*e),
                    (Scalar::Empty, _) => continue,
                    (Scalar::Text(_), ArgKind::Range) => continue,
                    _ => match s.to_bool() {
                        Ok(b) => b,
                        Err(e) => return Scalar::Err(e),
                    },
                };
                count += 1;
                any |= b;
                all &= b;
                odd ^= b;
            }
            if count == 0 {
                return Scalar::Err(XlError::Value);
            }
            Scalar::Bool(match name {
                "AND" => all,
                "OR" => any,
                _ => odd,
            })
        };
        Ok(Value::S(self.per_lane(&items, &f)))
    }

    /// Evaluate `f` over the item list for each lane (once if all items are uniform).
    pub(crate) fn per_lane(&self, items: &[(Lanes, ArgKind)], f: &dyn Fn(&mut dyn Iterator<Item = (Scalar, ArgKind)>) -> Scalar) -> Lanes {
        if items.iter().all(|(l, _)| l.is_uniform()) {
            return Lanes::Uniform(f(&mut items.iter().map(|(l, k)| (l.as_uniform().unwrap().clone(), *k))));
        }
        Lanes::from_vec((0..self.n).map(|i| f(&mut items.iter().map(|(l, k)| (l.get(i), *k)))).collect())
    }

    fn f_aggregate(&mut self, name: &str, args: &[Expr], cx: &Ctx) -> R<Value> {
        let items = self.agg_items(args, cx)?;
        // Fast path: packed numeric SUM/MIN/MAX/AVERAGE/COUNT over range items.
        let name = name.to_string();
        let f = move |vals: &mut dyn Iterator<Item = (Scalar, ArgKind)>| -> Scalar { aggregate(&name, vals) };
        Ok(Value::S(self.per_lane(&items, &f)))
    }

    fn f_sumproduct(&mut self, args: &[Expr], cx: &Ctx) -> R<Value> {
        let acx = Ctx { array: true, ..*cx };
        let mut arrays = Vec::new();
        for a in args {
            let v = self.eval(a, &acx)?;
            arrays.push(self.to_array(v)?);
        }
        let (rows, cols) = (arrays[0].rows, arrays[0].cols);
        if arrays.iter().any(|a| a.rows != rows || a.cols != cols) {
            return Ok(Value::err(XlError::Value));
        }
        let n = self.n;
        let lane = |i: usize| -> Scalar {
            let mut sum = 0.0;
            for k in 0..rows * cols {
                let mut p = 1.0;
                for a in &arrays {
                    match a.data[k].get(i) {
                        Scalar::Num(x) => p *= x,
                        Scalar::Err(e) => return Scalar::Err(e),
                        _ => p = 0.0,
                    }
                }
                sum += p;
            }
            Scalar::num(sum)
        };
        if arrays.iter().all(|a| a.all_uniform()) {
            return Ok(Value::S(Lanes::Uniform(lane(0))));
        }
        Ok(Value::S(Lanes::from_vec((0..n).map(lane).collect())))
    }

    fn f_concat(&mut self, name: &str, args: &[Expr], cx: &Ctx) -> R<Value> {
        let (delims, ignore_empty, rest) = if name == "TEXTJOIN" {
            if args.len() < 3 {
                return Ok(Value::err(XlError::Value));
            }
            let d = self.eval(&args[0], cx)?;
            let d = self.to_array(d)?;
            let ie = self.eval(&args[1], cx)?;
            let ie = self.to_lanes(ie, cx)?;
            let (Some(_), Some(ie)) = (d.data[0].as_uniform(), ie.as_uniform()) else {
                return unsupported("TEXTJOIN with case-dependent delimiter");
            };
            let delims: Vec<Scalar> = d.data.iter().map(|l| l.as_uniform().cloned().unwrap_or(Scalar::Empty)).collect();
            (Some(delims), ie.to_bool().unwrap_or(true), &args[2..])
        } else {
            (None, false, args)
        };
        let items = self.agg_items(rest, cx)?;
        let f = move |vals: &mut dyn Iterator<Item = (Scalar, ArgKind)>| -> Scalar {
            let mut out = String::new();
            let mut k = 0usize;
            for (s, _) in vals {
                let t = match s.to_text() {
                    Ok(t) => t,
                    Err(e) => return Scalar::Err(e),
                };
                if ignore_empty && t.is_empty() {
                    continue;
                }
                if let Some(d) = &delims {
                    if k > 0 {
                        out.push_str(&d[(k - 1) % d.len()].to_text().unwrap_or_default());
                    }
                }
                out.push_str(&t);
                k += 1;
            }
            if out.chars().count() > 32767 { Scalar::Err(XlError::Value) } else { Scalar::text(out) }
        };
        Ok(Value::S(self.per_lane(&items, &f)))
    }

    pub(crate) fn arr_value(&self, a: Array) -> Value {
        Value::A(Arc::new(a))
    }
}

/// Excel aggregate semantics over a mixed sequence of range and direct items.
pub(crate) fn aggregate(name: &str, vals: &mut dyn Iterator<Item = (Scalar, ArgKind)>) -> Scalar {
    let (mut sum, mut count, mut counta, mut blanks) = (0.0f64, 0usize, 0usize, 0usize);
    let (mut min, mut max, mut prod, mut sumsq) = (f64::INFINITY, f64::NEG_INFINITY, 1.0f64, 0.0f64);
    let mut all = Vec::new();
    for (s, kind) in vals {
        if !matches!(s, Scalar::Empty) {
            counta += 1;
        }
        if matches!(s, Scalar::Empty) || matches!(&s, Scalar::Text(t) if t.is_empty()) {
            blanks += 1;
        }
        let x = match (&s, kind) {
            (Scalar::Num(x), _) => *x,
            (Scalar::Err(e), _) => {
                if name == "COUNTA" || name == "COUNTBLANK" {
                    continue;
                }
                if name == "COUNT" {
                    continue;
                }
                return Scalar::Err(*e);
            }
            (Scalar::Empty, ArgKind::Range) | (Scalar::Text(_), ArgKind::Range) | (Scalar::Bool(_), ArgKind::Range) => continue,
            (Scalar::Empty, ArgKind::Direct) => {
                if name == "COUNT" || name == "AVERAGE" || name == "MIN" || name == "MAX" || name == "MEDIAN" {
                    continue;
                }
                0.0
            }
            (_, ArgKind::Direct) => match s.to_num() {
                Ok(x) => x,
                Err(e) => {
                    if name == "COUNT" || name == "COUNTA" || name == "COUNTBLANK" {
                        continue;
                    }
                    return Scalar::Err(e);
                }
            },
        };
        sum += x;
        count += 1;
        min = min.min(x);
        max = max.max(x);
        prod *= x;
        sumsq += x * x;
        if name == "MEDIAN" {
            all.push(x);
        }
    }
    match name {
        "SUM" => Scalar::num(sum),
        "COUNT" => Scalar::Num(count as f64),
        "COUNTA" => Scalar::Num(counta as f64),
        "COUNTBLANK" => Scalar::Num(blanks as f64),
        "MIN" => Scalar::num(if count == 0 { 0.0 } else { min }),
        "MAX" => Scalar::num(if count == 0 { 0.0 } else { max }),
        "AVERAGE" => {
            if count == 0 { Scalar::Err(XlError::Div0) } else { Scalar::num(sum / count as f64) }
        }
        "PRODUCT" => Scalar::num(if count == 0 { 0.0 } else { prod }),
        "SUMSQ" => Scalar::num(sumsq),
        "MEDIAN" => {
            if all.is_empty() {
                return Scalar::Err(XlError::Num);
            }
            all.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let m = all.len() / 2;
            Scalar::num(if all.len() % 2 == 1 { all[m] } else { (all[m - 1] + all[m]) / 2.0 })
        }
        _ => Scalar::Err(XlError::Value),
    }
}
