//! Regression/statistics functions and AGGREGATE.
use crate::eval::{unsupported, Ctx, Job, R};
use crate::funcs::{aggregate, ArgKind};
use crate::lanes::Lanes;
use crate::scalar::Scalar;
use crate::value::{Array, Value};
use std::sync::Arc;
use xlformula::Expr;
use xlmodel::XlError;

/// Least squares on pairs; `nums_only` skips pairs where either side is not a number.
fn fit(ys: &[Scalar], xs: &[Scalar], konst: bool, nums_only: bool) -> Result<(f64, f64), XlError> {
    let mut pts = Vec::with_capacity(ys.len());
    for (y, x) in ys.iter().zip(xs) {
        match (y, x) {
            (Scalar::Err(e), _) | (_, Scalar::Err(e)) => return Err(*e),
            (Scalar::Num(y), Scalar::Num(x)) => pts.push((*x, *y)),
            _ if nums_only => {}
            _ => return Err(XlError::Value),
        }
    }
    if pts.is_empty() {
        return Err(XlError::NA);
    }
    let n = pts.len() as f64;
    if konst {
        let mx = pts.iter().map(|p| p.0).sum::<f64>() / n;
        let my = pts.iter().map(|p| p.1).sum::<f64>() / n;
        let sxx: f64 = pts.iter().map(|p| (p.0 - mx) * (p.0 - mx)).sum();
        let sxy: f64 = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
        if sxx == 0.0 {
            return Err(XlError::Div0);
        }
        let b = sxy / sxx;
        Ok((b, my - b * mx))
    } else {
        let sxx: f64 = pts.iter().map(|p| p.0 * p.0).sum();
        let sxy: f64 = pts.iter().map(|p| p.0 * p.1).sum();
        if sxx == 0.0 {
            return Err(XlError::Div0);
        }
        Ok((sxy / sxx, 0.0))
    }
}

impl<'m> Job<'m> {
    pub(crate) fn call_stat(&mut self, name: &str, args: &[Expr], cx: &Ctx) -> R<Option<Value>> {
        let acx = Ctx { array: true, ..*cx };
        let n = self.n;
        Ok(Some(match name {
            "TREND" => {
                if args.is_empty() || args.len() > 4 {
                    return Ok(Some(Value::err(XlError::Value)));
                }
                let ky = {
                    let v = self.eval(&args[0], &acx)?;
                    self.to_array(v)?
                };
                let kx = match args.get(1) {
                    None | Some(Expr::Missing) => {
                        Arc::new(Array::new(ky.rows, ky.cols, (1..=ky.data.len()).map(|k| Lanes::Uniform(Scalar::Num(k as f64))).collect()))
                    }
                    Some(e) => {
                        let v = self.eval(e, &acx)?;
                        self.to_array(v)?
                    }
                };
                if kx.data.len() != ky.data.len() {
                    return if kx.rows == ky.rows && kx.cols > 1 || kx.cols == ky.cols && kx.rows > 1 {
                        unsupported("TREND with multiple regressors")
                    } else {
                        Ok(Some(Value::err(XlError::Ref)))
                    };
                }
                let nx = match args.get(2) {
                    None | Some(Expr::Missing) => kx.clone(),
                    Some(e) => {
                        let v = self.eval(e, &acx)?;
                        self.to_array(v)?
                    }
                };
                let konst = match args.get(3) {
                    None | Some(Expr::Missing) => true,
                    Some(e) => {
                        let v = self.eval(e, cx)?;
                        match self.to_lanes(v, cx)?.as_uniform() {
                            Some(s) => s.to_bool().unwrap_or(true),
                            None => return unsupported("case-dependent TREND const"),
                        }
                    }
                };
                let uniform = ky.all_uniform() && kx.all_uniform();
                let fits: Vec<Result<(f64, f64), XlError>> = (0..if uniform { 1 } else { n })
                    .map(|i| {
                        let ys: Vec<Scalar> = ky.data.iter().map(|l| l.get(i)).collect();
                        let xs: Vec<Scalar> = kx.data.iter().map(|l| l.get(i)).collect();
                        fit(&ys, &xs, konst, false)
                    })
                    .collect();
                let data = nx
                    .data
                    .iter()
                    .map(|x| {
                        let f = |i: usize| -> Scalar {
                            match (fits[if uniform { 0 } else { i }], x.with(i, |s| s.to_num())) {
                                (Err(e), _) | (_, Err(e)) => Scalar::Err(e),
                                (Ok((b, a)), Ok(x)) => Scalar::num(a + b * x),
                            }
                        };
                        if uniform && x.is_uniform() { Lanes::Uniform(f(0)) } else { Lanes::from_vec((0..n).map(f).collect()) }
                    })
                    .collect();
                Value::A(Arc::new(Array::new(nx.rows, nx.cols, data)))
            }
            "FORECAST" | "FORECAST.LINEAR" | "SLOPE" | "INTERCEPT" => {
                let off = if name.starts_with("FORECAST") { 1 } else { 0 };
                if args.len() != 2 + off {
                    return Ok(Some(Value::err(XlError::Value)));
                }
                let x = if off == 1 {
                    let v = self.eval(&args[0], cx)?;
                    Some(self.to_lanes(v, cx)?)
                } else {
                    None
                };
                let ky = {
                    let v = self.eval(&args[off], &acx)?;
                    self.to_array(v)?
                };
                let kx = {
                    let v = self.eval(&args[off + 1], &acx)?;
                    self.to_array(v)?
                };
                if ky.data.len() != kx.data.len() {
                    return Ok(Some(Value::err(XlError::NA)));
                }
                let uniform = ky.all_uniform() && kx.all_uniform() && x.as_ref().is_none_or(Lanes::is_uniform);
                let f = |i: usize| -> Scalar {
                    let ys: Vec<Scalar> = ky.data.iter().map(|l| l.get(i)).collect();
                    let xs: Vec<Scalar> = kx.data.iter().map(|l| l.get(i)).collect();
                    match fit(&ys, &xs, true, true) {
                        Err(e) => Scalar::Err(e),
                        Ok((b, a)) => match name {
                            "SLOPE" => Scalar::num(b),
                            "INTERCEPT" => Scalar::num(a),
                            _ => match x.as_ref().unwrap().with(i, |s| s.to_num()) {
                                Ok(x) => Scalar::num(a + b * x),
                                Err(e) => Scalar::Err(e),
                            },
                        },
                    }
                };
                Value::S(if uniform { Lanes::Uniform(f(0)) } else { Lanes::from_vec((0..n).map(f).collect()) })
            }
            "LARGE" | "SMALL" => {
                if args.len() != 2 {
                    return Ok(Some(Value::err(XlError::Value)));
                }
                let a = {
                    let v = self.eval(&args[0], &acx)?;
                    self.to_array(v)?
                };
                let k = {
                    let v = self.eval(&args[1], cx)?;
                    self.to_lanes(v, cx)?
                };
                let uniform = a.all_uniform() && k.is_uniform();
                let f = |i: usize| -> Scalar {
                    let mut v = Vec::new();
                    for l in &a.data {
                        match l.get(i) {
                            Scalar::Num(x) => v.push(x),
                            Scalar::Err(e) => return Scalar::Err(e),
                            _ => {}
                        }
                    }
                    kth(&mut v, k.get(i), name == "LARGE")
                };
                Value::S(if uniform { Lanes::Uniform(f(0)) } else { Lanes::from_vec((0..n).map(f).collect()) })
            }
            "STDEV" | "STDEV.S" | "STDEVP" | "STDEV.P" | "VAR" | "VAR.S" | "VARP" | "VAR.P" => {
                let items = self.agg_items(args, cx)?;
                let nm = name.to_string();
                let f = move |vals: &mut dyn Iterator<Item = (Scalar, ArgKind)>| -> Scalar { variance(&nm, vals) };
                Value::S(self.per_lane(&items, &f))
            }
            "AGGREGATE" => {
                if args.len() < 3 {
                    return Ok(Some(Value::err(XlError::Value)));
                }
                let fnum = match self.uniform_num(&args[0], cx)? {
                    Ok(v) => v as i32,
                    Err(e) => return Ok(Some(Value::err(e))),
                };
                let opt = match self.uniform_num(&args[1], cx)? {
                    Ok(v) => v as i32,
                    Err(e) => return Ok(Some(Value::err(e))),
                };
                let ignore_err = matches!(opt, 2 | 3 | 6 | 7);
                let items = self.agg_items(&args[2..if (14..=19).contains(&fnum) { 3 } else { args.len() }], cx)?;
                let items: Vec<(Lanes, ArgKind)> = items.into_iter().map(|(l, _)| (l, ArgKind::Range)).collect();
                let k = if (14..=19).contains(&fnum) {
                    let v = self.eval(args.get(3).unwrap_or(&Expr::Missing), cx)?;
                    Some(self.to_lanes(v, cx)?)
                } else {
                    None
                };
                let name = match fnum {
                    1 => "AVERAGE",
                    2 => "COUNT",
                    3 => "COUNTA",
                    4 => "MAX",
                    5 => "MIN",
                    6 => "PRODUCT",
                    7 => "STDEV",
                    8 => "STDEVP",
                    9 => "SUM",
                    10 => "VAR",
                    11 => "VARP",
                    12 => "MEDIAN",
                    14 => "LARGE",
                    15 => "SMALL",
                    _ => return unsupported(format!("AGGREGATE function {fnum}")),
                };
                let lanes = if items.iter().all(|(l, _)| l.is_uniform()) && k.as_ref().is_none_or(Lanes::is_uniform) { 1 } else { n };
                let f = |i: usize| -> Scalar {
                    let vals = items.iter().map(|(l, kd)| (l.get(i), *kd)).filter(|(s, _)| !(ignore_err && s.is_err()));
                    match name {
                        "LARGE" | "SMALL" => {
                            let mut v = Vec::new();
                            for (s, _) in vals {
                                match s {
                                    Scalar::Num(x) => v.push(x),
                                    Scalar::Err(e) => return Scalar::Err(e),
                                    _ => {}
                                }
                            }
                            kth(&mut v, k.as_ref().unwrap().get(i), name == "LARGE")
                        }
                        "STDEV" | "STDEVP" | "VAR" | "VARP" => variance(name, &mut vals.collect::<Vec<_>>().into_iter()),
                        _ => aggregate(name, &mut vals.collect::<Vec<_>>().into_iter()),
                    }
                };
                Value::S(if lanes == 1 { Lanes::Uniform(f(0)) } else { Lanes::from_vec((0..n).map(f).collect()) })
            }
            _ => return Ok(None),
        }))
    }
}

fn kth(v: &mut [f64], k: Scalar, large: bool) -> Scalar {
    let k = match k.to_num() {
        Ok(k) => k.ceil() as usize,
        Err(e) => return Scalar::Err(e),
    };
    if k < 1 || k > v.len() {
        return Scalar::Err(XlError::Num);
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Scalar::num(if large { v[v.len() - k] } else { v[k - 1] })
}

fn variance(name: &str, vals: &mut dyn Iterator<Item = (Scalar, ArgKind)>) -> Scalar {
    let mut xs = Vec::new();
    for (s, kind) in vals {
        match (&s, kind) {
            (Scalar::Num(x), _) => xs.push(*x),
            (Scalar::Err(e), _) => return Scalar::Err(*e),
            (_, ArgKind::Range) => {}
            (_, ArgKind::Direct) => match s.to_num() {
                Ok(x) => xs.push(x),
                Err(e) => return Scalar::Err(e),
            },
        }
    }
    let pop = name.ends_with('P') || name.ends_with(".P");
    let n = xs.len() as f64;
    if n < if pop { 1.0 } else { 2.0 } {
        return Scalar::Err(XlError::Div0);
    }
    let m = xs.iter().sum::<f64>() / n;
    let ss: f64 = xs.iter().map(|x| (x - m) * (x - m)).sum();
    let var = ss / if pop { n } else { n - 1.0 };
    Scalar::num(if name.starts_with("STDEV") { var.sqrt() } else { var })
}
