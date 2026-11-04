//! Dynamic-array and text-splitting functions.
use crate::eval::{unsupported, Ctx, Job, R};
use crate::lanes::Lanes;
use crate::scalar::{compare, Scalar};
use crate::value::{Array, Value};
use std::sync::Arc;
use xlformula::Expr;
use xlmodel::XlError;

fn na() -> Lanes {
    Lanes::Uniform(Scalar::Err(XlError::NA))
}

impl<'m> Job<'m> {
    fn arr_arg(&mut self, e: &Expr, cx: &Ctx) -> R<Arc<Array>> {
        let v = self.eval(e, &Ctx { array: true, ..*cx })?;
        self.to_array(v)
    }
    fn opt_uniform(&mut self, args: &[Expr], i: usize, cx: &Ctx) -> R<Option<Scalar>> {
        match args.get(i) {
            None | Some(Expr::Missing) => Ok(None),
            Some(e) => {
                let v = self.eval(e, cx)?;
                let l = self.to_lanes(v, cx)?;
                match l.as_uniform() {
                    Some(s) => Ok(Some(s.clone())),
                    None => unsupported("case-dependent shape argument"),
                }
            }
        }
    }
    fn opt_num(&mut self, args: &[Expr], i: usize, cx: &Ctx, d: f64) -> R<Result<f64, XlError>> {
        Ok(match self.opt_uniform(args, i, cx)? {
            None => Ok(d),
            Some(s) => s.to_num(),
        })
    }

    pub(crate) fn call_array(&mut self, name: &str, args: &[Expr], cx: &Ctx) -> R<Option<Value>> {
        macro_rules! num_or_err {
            ($e:expr) => {
                match $e {
                    Ok(v) => v,
                    Err(e) => return Ok(Some(Value::err(e))),
                }
            };
        }
        Ok(Some(match name {
            "TRANSPOSE" => {
                let a = self.arr_arg(&args[0], cx)?;
                let mut d = Vec::with_capacity(a.data.len());
                for c in 0..a.cols {
                    for r in 0..a.rows {
                        d.push(a.get(r, c).clone());
                    }
                }
                Value::A(Arc::new(Array::new(a.cols, a.rows, d)))
            }
            "VSTACK" | "HSTACK" => {
                let mut parts = Vec::new();
                for e in args {
                    parts.push(self.arr_arg(e, cx)?);
                }
                // Excel: a scalar error argument makes the whole result that error (per case).
                let n = self.n;
                let scalar_err = |i: usize| -> Option<Scalar> {
                    parts.iter().filter(|p| p.rows == 1 && p.cols == 1).find_map(|p| match p.data[0].get(i) {
                        e @ Scalar::Err(_) => Some(e),
                        _ => None,
                    })
                };
                let any_varying = parts.iter().any(|p| p.rows == 1 && p.cols == 1 && !p.data[0].is_uniform());
                if !any_varying {
                    if let Some(e) = scalar_err(0) {
                        return Ok(Some(Value::S(Lanes::Uniform(e))));
                    }
                } else if (0..n).any(|i| scalar_err(i).is_some()) {
                    let errs: Vec<Option<Scalar>> = (0..n).map(scalar_err).collect();
                    let poison = |l: &Lanes| -> Lanes { Lanes::from_vec((0..n).map(|i| errs[i].clone().unwrap_or_else(|| l.get(i))).collect()) };
                    parts = parts.iter().map(|p| Arc::new(Array::new(p.rows, p.cols, p.data.iter().map(poison).collect()))).collect();
                }
                if name == "VSTACK" {
                    let cols = parts.iter().map(|p| p.cols).max().unwrap_or(1);
                    let mut d = Vec::new();
                    let mut rows = 0;
                    for p in &parts {
                        for r in 0..p.rows {
                            for c in 0..cols {
                                d.push(if c < p.cols { p.get(r, c).clone() } else { na() });
                            }
                        }
                        rows += p.rows;
                    }
                    Value::A(Arc::new(Array::new(rows, cols, d)))
                } else {
                    let rows = parts.iter().map(|p| p.rows).max().unwrap_or(1);
                    let cols: usize = parts.iter().map(|p| p.cols).sum();
                    let mut d = Vec::with_capacity(rows * cols);
                    for r in 0..rows {
                        for p in &parts {
                            for c in 0..p.cols {
                                d.push(if r < p.rows { p.get(r, c).clone() } else { na() });
                            }
                        }
                    }
                    Value::A(Arc::new(Array::new(rows, cols, d)))
                }
            }
            "SEQUENCE" => {
                let rows = num_or_err!(self.opt_num(args, 0, cx, 1.0)?) as usize;
                let cols = num_or_err!(self.opt_num(args, 1, cx, 1.0)?) as usize;
                let start = num_or_err!(self.opt_num(args, 2, cx, 1.0)?);
                let step = num_or_err!(self.opt_num(args, 3, cx, 1.0)?);
                if rows == 0 || cols == 0 {
                    return Ok(Some(Value::err(XlError::Calc)));
                }
                let d = (0..rows * cols).map(|k| Lanes::Uniform(Scalar::num(start + step * k as f64))).collect();
                Value::A(Arc::new(Array::new(rows, cols, d)))
            }
            "TOCOL" | "TOROW" => {
                let a = self.arr_arg(&args[0], cx)?;
                let ignore = num_or_err!(self.opt_num(args, 1, cx, 0.0)?) as i32;
                let by_col = self.opt_uniform(args, 2, cx)?.map(|s| s.to_bool().unwrap_or(false)).unwrap_or(false);
                let mut d = Vec::new();
                let order: Vec<(usize, usize)> = if by_col {
                    (0..a.cols).flat_map(|c| (0..a.rows).map(move |r| (r, c))).collect()
                } else {
                    (0..a.rows).flat_map(|r| (0..a.cols).map(move |c| (r, c))).collect()
                };
                for (r, c) in order {
                    let l = a.get(r, c);
                    let skip = match (ignore, l.as_uniform()) {
                        (1 | 3, Some(Scalar::Empty)) => true,
                        (2 | 3, Some(Scalar::Err(_))) => true,
                        (0, _) => false,
                        (_, None) => return unsupported("TOCOL ignore with case-dependent values"),
                        _ => false,
                    };
                    if !skip {
                        d.push(l.clone());
                    }
                }
                if d.is_empty() {
                    return Ok(Some(Value::err(XlError::Calc)));
                }
                let k = d.len();
                Value::A(Arc::new(if name == "TOCOL" { Array::new(k, 1, d) } else { Array::new(1, k, d) }))
            }
            "FILTER" => {
                let a = self.arr_arg(&args[0], cx)?;
                let inc = self.arr_arg(&args[1], cx)?;
                if !inc.all_uniform() {
                    return unsupported("FILTER with case-dependent condition");
                }
                let by_rows = inc.cols == 1 && inc.rows == a.rows;
                let by_cols = inc.rows == 1 && inc.cols == a.cols;
                if !by_rows && !by_cols {
                    return Ok(Some(Value::err(XlError::Value)));
                }
                let keep: Vec<bool> = inc.data.iter().map(|l| l.as_uniform().unwrap().to_bool().unwrap_or(false)).collect();
                let mut d = Vec::new();
                let mut rows = 0;
                let cols;
                if by_rows {
                    cols = a.cols;
                    for r in 0..a.rows {
                        if keep[r] {
                            rows += 1;
                            d.extend((0..a.cols).map(|c| a.get(r, c).clone()));
                        }
                    }
                } else {
                    rows = a.rows;
                    let idx: Vec<usize> = (0..a.cols).filter(|c| keep[*c]).collect();
                    cols = idx.len();
                    for r in 0..a.rows {
                        d.extend(idx.iter().map(|c| a.get(r, *c).clone()));
                    }
                }
                if d.is_empty() {
                    return Ok(Some(match args.get(2) {
                        Some(e) if !matches!(e, Expr::Missing) => self.eval(e, cx)?,
                        _ => Value::err(XlError::Calc),
                    }));
                }
                Value::A(Arc::new(Array::new(rows, cols, d)))
            }
            "SORT" => {
                let a = self.arr_arg(&args[0], cx)?;
                if !a.all_uniform() {
                    return unsupported("SORT of case-dependent values");
                }
                let idx = num_or_err!(self.opt_num(args, 1, cx, 1.0)?) as usize;
                let order = num_or_err!(self.opt_num(args, 2, cx, 1.0)?);
                let by_col = self.opt_uniform(args, 3, cx)?.map(|s| s.to_bool().unwrap_or(false)).unwrap_or(false);
                if by_col {
                    return unsupported("SORT by_col");
                }
                if idx < 1 || idx > a.cols {
                    return Ok(Some(Value::err(XlError::Value)));
                }
                let mut rows: Vec<usize> = (0..a.rows).collect();
                rows.sort_by(|&x, &y| {
                    let o = sort_cmp(a.get(x, idx - 1).as_uniform().unwrap(), a.get(y, idx - 1).as_uniform().unwrap());
                    if order < 0.0 { o.reverse() } else { o }
                });
                let d = rows.iter().flat_map(|&r| (0..a.cols).map(move |c| (r, c))).map(|(r, c)| a.get(r, c).clone()).collect();
                Value::A(Arc::new(Array::new(a.rows, a.cols, d)))
            }
            "UNIQUE" => {
                let a = self.arr_arg(&args[0], cx)?;
                if !a.all_uniform() {
                    return unsupported("UNIQUE of case-dependent values");
                }
                let by_col = self.opt_uniform(args, 1, cx)?.map(|s| s.to_bool().unwrap_or(false)).unwrap_or(false);
                let once = self.opt_uniform(args, 2, cx)?.map(|s| s.to_bool().unwrap_or(false)).unwrap_or(false);
                if by_col {
                    return unsupported("UNIQUE by_col");
                }
                let rowkey = |r: usize| -> Vec<Option<crate::lookup::Key>> { (0..a.cols).map(|c| crate::lookup::key(a.get(r, c).as_uniform().unwrap()).or(None)).collect() };
                let keys: Vec<_> = (0..a.rows).map(rowkey).collect();
                let mut d = Vec::new();
                let mut rows = 0;
                for r in 0..a.rows {
                    let count = keys.iter().filter(|k| **k == keys[r]).count();
                    let first = keys.iter().position(|k| *k == keys[r]) == Some(r);
                    if (once && count == 1) || (!once && first) {
                        rows += 1;
                        d.extend((0..a.cols).map(|c| a.get(r, c).clone()));
                    }
                }
                if rows == 0 {
                    return Ok(Some(Value::err(XlError::Calc)));
                }
                Value::A(Arc::new(Array::new(rows, a.cols, d)))
            }
            "TAKE" | "DROP" => {
                let a = self.arr_arg(&args[0], cx)?;
                let r = self.opt_uniform(args, 1, cx)?.map(|s| s.to_num().unwrap_or(0.0) as isize);
                let c = self.opt_uniform(args, 2, cx)?.map(|s| s.to_num().unwrap_or(0.0) as isize);
                let span = |len: usize, k: Option<isize>, take: bool| -> (usize, usize) {
                    match k {
                        None => (0, len),
                        Some(k) => {
                            let m = (k.unsigned_abs()).min(len);
                            match (take, k >= 0) {
                                (true, true) => (0, m),
                                (true, false) => (len - m, len),
                                (false, true) => (m, len),
                                (false, false) => (0, len - m),
                            }
                        }
                    }
                };
                let take = name == "TAKE";
                let (r0, r1) = span(a.rows, r, take);
                let (c0, c1) = span(a.cols, c, take);
                if r1 <= r0 || c1 <= c0 {
                    return Ok(Some(Value::err(XlError::Calc)));
                }
                let d = (r0..r1).flat_map(|r| (c0..c1).map(move |c| (r, c))).map(|(r, c)| a.get(r, c).clone()).collect();
                Value::A(Arc::new(Array::new(r1 - r0, c1 - c0, d)))
            }
            "CHOOSECOLS" | "CHOOSEROWS" => {
                let a = self.arr_arg(&args[0], cx)?;
                let mut picks = Vec::new();
                for i in 1..args.len() {
                    let k = num_or_err!(self.opt_num(args, i, cx, 1.0)?) as isize;
                    let len = if name == "CHOOSECOLS" { a.cols } else { a.rows } as isize;
                    let k = if k < 0 { len + k + 1 } else { k };
                    if k < 1 || k > len {
                        return Ok(Some(Value::err(XlError::Value)));
                    }
                    picks.push(k as usize - 1);
                }
                let d: Vec<Lanes> = if name == "CHOOSECOLS" {
                    (0..a.rows).flat_map(|r| picks.iter().map(move |&c| (r, c))).map(|(r, c)| a.get(r, c).clone()).collect()
                } else {
                    picks.iter().flat_map(|&r| (0..a.cols).map(move |c| (r, c))).map(|(r, c)| a.get(r, c).clone()).collect()
                };
                let (rows, cols) = if name == "CHOOSECOLS" { (a.rows, picks.len()) } else { (picks.len(), a.cols) };
                Value::A(Arc::new(Array::new(rows, cols, d)))
            }
            "TEXTBEFORE" | "TEXTAFTER" => {
                let mut vals = Vec::new();
                for (i, a) in args.iter().enumerate().take(6) {
                    vals.push(match a {
                        Expr::Missing => Value::S(Lanes::Uniform(match i {
                            2 => Scalar::Num(1.0),
                            3 | 4 => Scalar::Num(0.0),
                            _ => Scalar::Err(XlError::NA),
                        })),
                        e => self.eval(e, cx)?,
                    });
                }
                while vals.len() < 6 {
                    let i = vals.len();
                    vals.push(Value::S(Lanes::Uniform(match i {
                        2 => Scalar::Num(1.0),
                        3 | 4 => Scalar::Num(0.0),
                        _ => Scalar::Err(XlError::NA),
                    })));
                }
                let after = name == "TEXTAFTER";
                let n = self.n;
                self.lift(vals, cx, &move |l| crate::funcs::lanewise(n, l, &|s| text_split_one(s, after)))?
            }
            "TEXTSPLIT" => {
                let t = self.eval(&args[0], cx)?;
                let t = self.to_lanes(t, cx)?;
                let Some(text) = t.as_uniform().cloned() else { return unsupported("TEXTSPLIT of case-dependent text") };
                let text = match text.to_text() {
                    Ok(t) => t,
                    Err(e) => return Ok(Some(Value::err(e))),
                };
                let delims = |job: &mut Self, i: usize| -> R<Vec<String>> {
                    match args.get(i) {
                        None | Some(Expr::Missing) => Ok(vec![]),
                        Some(e) => {
                            let a = job.arr_arg(e, cx)?;
                            Ok(a.data.iter().filter_map(|l| l.as_uniform().and_then(|s| s.to_text().ok())).map(|s| s.to_string()).collect())
                        }
                    }
                };
                let cd = delims(self, 1)?;
                let rd = delims(self, 2)?;
                let ignore = self.opt_uniform(args, 3, cx)?.map(|s| s.to_bool().unwrap_or(false)).unwrap_or(false);
                let ci = num_or_err!(self.opt_num(args, 4, cx, 0.0)?) == 1.0;
                let pad = self.opt_uniform(args, 5, cx)?.unwrap_or(Scalar::Err(XlError::NA));
                let split = |s: &str, ds: &[String]| -> Vec<String> {
                    if ds.is_empty() {
                        return vec![s.to_string()];
                    }
                    let mut out = vec![];
                    let mut cur = String::new();
                    let mut i = 0;
                    let b = s;
                    'outer: while i < b.len() {
                        for d in ds.iter().filter(|d| !d.is_empty()) {
                            let hit = if ci { b[i..].to_lowercase().starts_with(&d.to_lowercase()) } else { b[i..].starts_with(d.as_str()) };
                            if hit {
                                out.push(std::mem::take(&mut cur));
                                i += d.len();
                                continue 'outer;
                            }
                        }
                        let ch = b[i..].chars().next().unwrap();
                        cur.push(ch);
                        i += ch.len_utf8();
                    }
                    out.push(cur);
                    if ignore {
                        out.retain(|x| !x.is_empty());
                    }
                    out
                };
                let rows: Vec<Vec<String>> = split(&text, &rd).iter().map(|r| split(r, &cd)).collect();
                let cols = rows.iter().map(|r| r.len()).max().unwrap_or(1).max(1);
                let nrows = rows.len().max(1);
                let mut d = Vec::with_capacity(nrows * cols);
                for r in &rows {
                    for c in 0..cols {
                        d.push(Lanes::Uniform(r.get(c).map(|s| Scalar::text(s.as_str())).unwrap_or_else(|| pad.clone())));
                    }
                }
                Value::A(Arc::new(Array::new(rows.len(), cols, d)))
            }
            _ => return Ok(None),
        }))
    }
}

fn sort_cmp(a: &Scalar, b: &Scalar) -> std::cmp::Ordering {
    // Errors sort last; blanks after everything else.
    match (a, b) {
        (Scalar::Empty, Scalar::Empty) => std::cmp::Ordering::Equal,
        (Scalar::Empty, _) => std::cmp::Ordering::Greater,
        (_, Scalar::Empty) => std::cmp::Ordering::Less,
        _ => compare(a, b),
    }
}

/// TEXTBEFORE / TEXTAFTER on one lane: (text, delimiter, instance, match_mode, match_end, if_not_found).
fn text_split_one(s: &[Scalar], after: bool) -> Scalar {
    let text = match s[0].to_text() {
        Ok(t) => t,
        Err(e) => return Scalar::Err(e),
    };
    let delim = match s[1].to_text() {
        Ok(t) => t,
        Err(e) => return Scalar::Err(e),
    };
    let (Ok(inst), Ok(mm), Ok(me)) = (s[2].to_num(), s[3].to_num(), s[4].to_num()) else { return Scalar::Err(XlError::Value) };
    let inst = inst.trunc() as i64;
    if inst == 0 || delim.is_empty() && inst.abs() > 1 {
        return Scalar::Err(XlError::Value);
    }
    let (h, d) = if mm == 1.0 { (text.to_lowercase(), delim.to_lowercase()) } else { (text.to_string(), delim.to_string()) };
    let mut hits: Vec<usize> = if d.is_empty() { vec![] } else { h.match_indices(&d).map(|(i, _)| i).collect() };
    if me == 1.0 {
        hits.push(h.len());
    }
    let pick = if inst > 0 { hits.get(inst as usize - 1).copied() } else { hits.len().checked_sub(inst.unsigned_abs() as usize).and_then(|k| hits.get(k).copied()) };
    let pos = if d.is_empty() { Some(if inst > 0 { 0 } else { h.len() }) } else { pick };
    match pos {
        Some(p) => {
            if after {
                let start = (p + d.len()).min(text.len());
                Scalar::text(&text[start..])
            } else {
                Scalar::text(&text[..p])
            }
        }
        None => match &s[5] {
            Scalar::Err(XlError::NA) => Scalar::Err(XlError::NA),
            v => v.clone(),
        },
    }
}
