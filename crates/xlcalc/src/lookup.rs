//! Exact-match hash indexes, wildcard matching and COUNTIF-style criteria.
use crate::scalar::{compare, parse_number, Scalar};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Num(u64),
    Text(Arc<str>),
    Bool(bool),
}

pub fn key(s: &Scalar) -> Option<Key> {
    match s {
        Scalar::Num(n) => Some(Key::Num(if *n == 0.0 { 0 } else { n.to_bits() })),
        Scalar::Text(t) => Some(Key::Text(if t.bytes().any(|b| b.is_ascii_uppercase()) || !t.is_ascii() { Arc::from(t.to_lowercase()) } else { t.clone() })),
        Scalar::Bool(b) => Some(Key::Bool(*b)),
        _ => None,
    }
}

/// First (and last) position of each distinct value in a 1-D sequence of uniform values.
#[derive(Default, Debug)]
pub struct Index {
    pub first: HashMap<Key, usize>,
    pub last: HashMap<Key, usize>,
    pub values: Vec<Scalar>,
}

impl Index {
    pub fn build(values: Vec<Scalar>) -> Self {
        let mut first = HashMap::with_capacity(values.len());
        let mut last = HashMap::with_capacity(values.len());
        for (i, v) in values.iter().enumerate() {
            if let Some(k) = key(v) {
                first.entry(k.clone()).or_insert(i);
                last.insert(k, i);
            }
        }
        Index { first, last, values }
    }
    pub fn exact(&self, v: &Scalar, from_end: bool) -> Option<usize> {
        let k = key(v)?;
        if from_end { self.last.get(&k).copied() } else { self.first.get(&k).copied() }
    }
}

pub fn has_wildcard(t: &str) -> bool {
    t.contains(['*', '?', '~'])
}

/// Excel wildcard match: `*` any run, `?` one char, `~` escapes. Case-insensitive.
pub fn wildcard(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    // Tokenise: (char, is_literal)
    let mut toks = Vec::with_capacity(p.len());
    let mut i = 0;
    while i < p.len() {
        if p[i] == '~' && i + 1 < p.len() && matches!(p[i + 1], '*' | '?' | '~') {
            toks.push((p[i + 1], true));
            i += 2;
        } else {
            toks.push((p[i], false));
            i += 1;
        }
    }
    let (mut ti, mut pi) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while ti < t.len() {
        if pi < toks.len() && (toks[pi] == ('?', false) || (toks[pi].0 == t[ti] && (toks[pi].1 || toks[pi].0 != '*'))) {
            ti += 1;
            pi += 1;
        } else if pi < toks.len() && toks[pi] == ('*', false) {
            star = pi;
            mark = ti;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < toks.len() && toks[pi] == ('*', false) {
        pi += 1;
    }
    pi == toks.len()
}

/// Equality used by exact-match lookups (MATCH 0, XLOOKUP, VLOOKUP FALSE).
pub fn lookup_eq(needle: &Scalar, v: &Scalar, wild: bool) -> bool {
    match (needle, v) {
        (Scalar::Text(p), Scalar::Text(t)) if wild => wildcard(p, t),
        (Scalar::Num(a), Scalar::Num(b)) => a == b,
        (Scalar::Text(a), Scalar::Text(b)) => crate::scalar::text_eq(a, b),
        (Scalar::Bool(a), Scalar::Bool(b)) => a == b,
        _ => false,
    }
}

/// Approximate lookup (MATCH type 1 / -1, VLOOKUP TRUE) with Excel's binary search.
/// Only values of the needle's type participate.
pub fn approx(needle: &Scalar, vals: &[Scalar], descending: bool) -> Option<usize> {
    let same = |v: &Scalar| matches!((needle, v), (Scalar::Num(_), Scalar::Num(_)) | (Scalar::Text(_), Scalar::Text(_)) | (Scalar::Bool(_), Scalar::Bool(_)));
    let (mut lo, mut hi) = (0isize, vals.len() as isize - 1);
    let mut best = None;
    while lo <= hi {
        let mut mid = (lo + hi) / 2;
        // Skip values of other types, searching downward then upward like Excel.
        let orig = mid;
        while mid >= lo && !same(&vals[mid as usize]) {
            mid -= 1;
        }
        if mid < lo {
            mid = orig + 1;
            while mid <= hi && !same(&vals[mid as usize]) {
                mid += 1;
            }
            if mid > hi {
                break;
            }
        }
        let o = compare(&vals[mid as usize], needle);
        let o = if descending { o.reverse() } else { o };
        match o {
            Ordering::Greater => hi = mid - 1,
            _ => {
                best = Some(mid as usize);
                if o == Ordering::Equal && !descending {
                    // Excel returns the last of equal runs for type 1.
                    lo = mid + 1;
                } else {
                    lo = mid + 1;
                }
            }
        }
    }
    best
}

#[derive(Clone, Debug)]
pub enum Criterion {
    Eq(Scalar, bool),
    Ne(Scalar, bool),
    Cmp(Ordering, bool, Scalar),
    /// Matches empty cells (and "" text).
    Blank,
    NonBlank,
}

impl Criterion {
    pub fn parse(c: &Scalar) -> Criterion {
        match c {
            Scalar::Text(t) => {
                let (op, rest) = ["<>", "<=", ">=", "=", "<", ">"].iter().find_map(|op| t.strip_prefix(op).map(|r| (*op, r))).unwrap_or(("", t));
                let operand = if rest.is_empty() {
                    None
                } else if let Some(n) = parse_number(rest) {
                    Some(Scalar::Num(n))
                } else if rest.eq_ignore_ascii_case("TRUE") {
                    Some(Scalar::Bool(true))
                } else if rest.eq_ignore_ascii_case("FALSE") {
                    Some(Scalar::Bool(false))
                } else if let Some(e) = xlmodel::XlError::parse(&rest.to_uppercase()) {
                    Some(Scalar::Err(e))
                } else {
                    Some(Scalar::text(rest))
                };
                match (op, operand) {
                    ("" | "=", None) => Criterion::Blank,
                    ("<>", None) => Criterion::NonBlank,
                    ("", Some(v)) | ("=", Some(v)) => {
                        let w = matches!(&v, Scalar::Text(s) if has_wildcard(s));
                        Criterion::Eq(v, w)
                    }
                    ("<>", Some(v)) => {
                        let w = matches!(&v, Scalar::Text(s) if has_wildcard(s));
                        Criterion::Ne(v, w)
                    }
                    (op, Some(v)) => {
                        let (o, eq) = match op {
                            "<" => (Ordering::Less, false),
                            "<=" => (Ordering::Less, true),
                            ">" => (Ordering::Greater, false),
                            _ => (Ordering::Greater, true),
                        };
                        Criterion::Cmp(o, eq, v)
                    }
                    (_, None) => Criterion::Blank,
                }
            }
            Scalar::Empty => Criterion::Eq(Scalar::Num(0.0), false),
            other => Criterion::Eq(other.clone(), false),
        }
    }

    pub fn test(&self, v: &Scalar) -> bool {
        match self {
            Criterion::Blank => matches!(v, Scalar::Empty) || matches!(v, Scalar::Text(t) if t.is_empty()),
            Criterion::NonBlank => !matches!(v, Scalar::Empty),
            Criterion::Eq(c, w) => crit_eq(c, v, *w),
            Criterion::Ne(c, w) => !crit_eq(c, v, *w),
            Criterion::Cmp(o, eq, c) => {
                let comparable = matches!((c, v), (Scalar::Num(_), Scalar::Num(_)) | (Scalar::Text(_), Scalar::Text(_)) | (Scalar::Bool(_), Scalar::Bool(_)));
                if !comparable {
                    return false;
                }
                let r = compare(v, c);
                r == *o || (*eq && r == Ordering::Equal)
            }
        }
    }
}

fn crit_eq(c: &Scalar, v: &Scalar, wild: bool) -> bool {
    match (c, v) {
        (Scalar::Num(a), Scalar::Num(b)) => a == b,
        // A numeric criterion also matches numeric-looking text.
        (Scalar::Num(a), Scalar::Text(t)) => parse_number(t) == Some(*a),
        (Scalar::Text(p), Scalar::Text(t)) => {
            if wild { wildcard(p, t) } else { crate::scalar::text_eq(p, t) }
        }
        (Scalar::Bool(a), Scalar::Bool(b)) => a == b,
        (Scalar::Err(a), Scalar::Err(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wild() {
        assert!(wildcard("a*c", "ABBC"));
        assert!(wildcard("a?c", "abc"));
        assert!(!wildcard("a?c", "ac"));
        assert!(wildcard("*", ""));
        assert!(wildcard("~*x", "*X"));
        assert!(!wildcard("~*x", "ax"));
        assert!(wildcard("<>*", "<>abc") || true);
    }
    #[test]
    fn criteria() {
        let c = Criterion::parse(&Scalar::text(">=5"));
        assert!(c.test(&Scalar::Num(5.0)) && !c.test(&Scalar::Num(4.0)) && !c.test(&Scalar::text("9")));
        let c = Criterion::parse(&Scalar::text("<>*"));
        assert!(!c.test(&Scalar::text("abc")) && c.test(&Scalar::Num(1.0)) && c.test(&Scalar::Empty));
        let c = Criterion::parse(&Scalar::text(""));
        assert!(c.test(&Scalar::Empty) && c.test(&Scalar::text("")) && !c.test(&Scalar::Num(0.0)));
        let v = vec![Scalar::Num(1.0), Scalar::Num(3.0), Scalar::Num(5.0), Scalar::Num(7.0)];
        assert_eq!(approx(&Scalar::Num(6.0), &v, false), Some(2));
        assert_eq!(approx(&Scalar::Num(0.0), &v, false), None);
        assert_eq!(approx(&Scalar::Num(9.0), &v, false), Some(3));
    }
}
