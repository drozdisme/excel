//! A value across all cases ("lanes") of a batch: uniform, packed numbers, or mixed.
use crate::scalar::Scalar;
use std::sync::Arc;
use xlmodel::XlError;

#[derive(Clone, Debug)]
pub enum Lanes {
    Uniform(Scalar),
    /// Every lane is a finite number.
    Num(Arc<[f64]>),
    Mixed(Arc<[Scalar]>),
}

impl Lanes {
    pub fn uniform(s: Scalar) -> Self {
        Lanes::Uniform(s)
    }
    pub fn is_uniform(&self) -> bool {
        matches!(self, Lanes::Uniform(_))
    }
    pub fn as_uniform(&self) -> Option<&Scalar> {
        match self {
            Lanes::Uniform(s) => Some(s),
            _ => None,
        }
    }
    pub fn get(&self, i: usize) -> Scalar {
        match self {
            Lanes::Uniform(s) => s.clone(),
            Lanes::Num(v) => Scalar::Num(v[i]),
            Lanes::Mixed(v) => v[i].clone(),
        }
    }
    /// Borrowing access for non-numeric lanes; numbers are returned by value.
    #[inline]
    pub fn with<T>(&self, i: usize, f: impl FnOnce(&Scalar) -> T) -> T {
        match self {
            Lanes::Uniform(s) => f(s),
            Lanes::Num(v) => f(&Scalar::Num(v[i])),
            Lanes::Mixed(v) => f(&v[i]),
        }
    }
    /// Build from per-lane scalars, compacting to Uniform/Num when possible.
    pub fn from_vec(v: Vec<Scalar>) -> Self {
        if v.is_empty() {
            return Lanes::Uniform(Scalar::Empty);
        }
        if v.iter().all(|s| matches!(s, Scalar::Num(_))) {
            let nums: Vec<f64> = v.iter().map(|s| if let Scalar::Num(n) = s { *n } else { 0.0 }).collect();
            return Lanes::from_nums(nums);
        }
        if v.iter().skip(1).all(|s| *s == v[0]) {
            return Lanes::Uniform(v.into_iter().next().unwrap());
        }
        Lanes::Mixed(v.into())
    }
    pub fn from_nums(v: Vec<f64>) -> Self {
        if v.iter().any(|x| !x.is_finite()) {
            return Lanes::from_vec(v.into_iter().map(Scalar::num).collect());
        }
        if v.iter().skip(1).all(|x| x.to_bits() == v[0].to_bits()) {
            return Lanes::Uniform(Scalar::Num(v[0]));
        }
        Lanes::Num(v.into())
    }
    pub fn to_vec(&self, n: usize) -> Vec<Scalar> {
        (0..n).map(|i| self.get(i)).collect()
    }
    pub fn any_err(&self) -> bool {
        match self {
            Lanes::Uniform(s) => s.is_err(),
            Lanes::Num(_) => false,
            Lanes::Mixed(v) => v.iter().any(Scalar::is_err),
        }
    }

    /// Apply a scalar function per lane (once when uniform).
    pub fn map(&self, n: usize, f: impl Fn(&Scalar) -> Scalar) -> Lanes {
        match self {
            Lanes::Uniform(s) => Lanes::Uniform(f(s)),
            _ => Lanes::from_vec((0..n).map(|i| self.with(i, &f)).collect()),
        }
    }

    /// Numeric unary function with a packed fast path.
    pub fn map_num(&self, n: usize, f: impl Fn(f64) -> Result<f64, XlError>) -> Lanes {
        let scalar = |s: &Scalar| match s.to_num().and_then(&f) {
            Ok(v) => Scalar::num(v),
            Err(e) => Scalar::Err(e),
        };
        match self {
            Lanes::Num(v) => {
                let mut out = Vec::with_capacity(v.len());
                for &x in v.iter() {
                    match f(x) {
                        Ok(y) if y.is_finite() => out.push(y),
                        _ => return self.map(n, scalar),
                    }
                }
                Lanes::from_nums(out)
            }
            _ => self.map(n, scalar),
        }
    }
}

/// Apply a binary scalar function lane-wise with broadcasting of uniform sides.
pub fn zip2(a: &Lanes, b: &Lanes, n: usize, f: impl Fn(&Scalar, &Scalar) -> Scalar) -> Lanes {
    match (a, b) {
        (Lanes::Uniform(x), Lanes::Uniform(y)) => Lanes::Uniform(f(x, y)),
        _ => Lanes::from_vec((0..n).map(|i| a.with(i, |x| b.with(i, |y| f(x, y)))).collect()),
    }
}

/// Numeric binary operation with a packed fast path; `f` returns NaN/inf for errors and
/// `slow` gives the exact scalar semantics when the fast path cannot be used.
pub fn zip2_num(
    a: &Lanes,
    b: &Lanes,
    n: usize,
    fast: impl Fn(f64, f64) -> f64,
    slow: impl Fn(&Scalar, &Scalar) -> Scalar,
) -> Lanes {
    let num_of = |l: &Lanes| -> Option<Result<f64, ()>> {
        match l {
            Lanes::Uniform(Scalar::Num(x)) => Some(Ok(*x)),
            Lanes::Num(_) => Some(Err(())),
            _ => None,
        }
    };
    match (num_of(a), num_of(b)) {
        (Some(_), Some(_)) if !(a.is_uniform() && b.is_uniform()) => {
            let mut out = Vec::with_capacity(n);
            match (a, b) {
                (Lanes::Num(x), Lanes::Num(y)) => {
                    for i in 0..n {
                        out.push(fast(x[i], y[i]));
                    }
                }
                (Lanes::Num(x), Lanes::Uniform(Scalar::Num(c))) => {
                    for i in 0..n {
                        out.push(fast(x[i], *c));
                    }
                }
                (Lanes::Uniform(Scalar::Num(c)), Lanes::Num(y)) => {
                    for i in 0..n {
                        out.push(fast(*c, y[i]));
                    }
                }
                _ => unreachable!(),
            }
            if out.iter().all(|v| v.is_finite()) {
                return Lanes::from_nums(out);
            }
            zip2(a, b, n, slow)
        }
        _ => zip2(a, b, n, slow),
    }
}

/// Lane-wise select: where `cond` lane is true take `t`, else `f`. Errors in cond propagate.
pub fn select(cond: &Lanes, t: &Lanes, f: &Lanes, n: usize) -> Lanes {
    Lanes::from_vec(
        (0..n)
            .map(|i| match cond.with(i, |c| c.to_bool()) {
                Ok(true) => t.get(i),
                Ok(false) => f.get(i),
                Err(e) => Scalar::Err(e),
            })
            .collect(),
    )
}
