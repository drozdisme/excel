//! Excel scalar values and their coercion rules.
use std::cmp::Ordering;
use std::sync::Arc;
use xlmodel::{CellValue, XlError};

#[derive(Clone, Debug, PartialEq, Default)]
pub enum Scalar {
    #[default]
    Empty,
    Num(f64),
    Text(Arc<str>),
    Bool(bool),
    Err(XlError),
}

impl From<&CellValue> for Scalar {
    fn from(v: &CellValue) -> Self {
        match v {
            CellValue::Empty => Scalar::Empty,
            CellValue::Number(n) => Scalar::Num(*n),
            CellValue::Text(t) => Scalar::Text(t.clone()),
            CellValue::Bool(b) => Scalar::Bool(*b),
            CellValue::Error(e) => Scalar::Err(*e),
        }
    }
}

impl Scalar {
    pub fn text(s: impl Into<Arc<str>>) -> Self {
        Scalar::Text(s.into())
    }
    pub fn is_err(&self) -> bool {
        matches!(self, Scalar::Err(_))
    }
    /// Result of a finished numeric computation: non-finite becomes #NUM!.
    pub fn num(v: f64) -> Self {
        if v.is_finite() { Scalar::Num(if v == 0.0 { 0.0 } else { v }) } else { Scalar::Err(XlError::Num) }
    }

    /// Coercion used by arithmetic operators and numeric parameters.
    pub fn to_num(&self) -> Result<f64, XlError> {
        match self {
            Scalar::Empty => Ok(0.0),
            Scalar::Num(n) => Ok(*n),
            Scalar::Bool(b) => Ok(*b as u8 as f64),
            Scalar::Text(t) => parse_number(t).ok_or(XlError::Value),
            Scalar::Err(e) => Err(*e),
        }
    }

    /// Coercion used by `&` and text parameters.
    pub fn to_text(&self) -> Result<Arc<str>, XlError> {
        match self {
            Scalar::Empty => Ok(Arc::from("")),
            Scalar::Num(n) => Ok(Arc::from(number_to_text(*n))),
            Scalar::Text(t) => Ok(t.clone()),
            Scalar::Bool(b) => Ok(Arc::from(if *b { "TRUE" } else { "FALSE" })),
            Scalar::Err(e) => Err(*e),
        }
    }

    /// Coercion used by logical parameters (IF condition, AND/OR arguments).
    pub fn to_bool(&self) -> Result<bool, XlError> {
        match self {
            Scalar::Empty => Ok(false),
            Scalar::Num(n) => Ok(*n != 0.0),
            Scalar::Bool(b) => Ok(*b),
            Scalar::Text(t) => {
                if t.eq_ignore_ascii_case("TRUE") {
                    Ok(true)
                } else if t.eq_ignore_ascii_case("FALSE") {
                    Ok(false)
                } else {
                    Err(XlError::Value)
                }
            }
            Scalar::Err(e) => Err(*e),
        }
    }
}

/// Parse text the way Excel coerces it to a number (VALUE-like, invariant en-US).
pub fn parse_number(t: &str) -> Option<f64> {
    let s = t.trim();
    if s.is_empty() {
        return None;
    }
    let (neg, s) = if let Some(r) = s.strip_prefix('(').and_then(|r| r.strip_suffix(')')) { (true, r.trim()) } else { (false, s) };
    let (pct, s) = match s.strip_suffix('%') {
        Some(r) => (true, r.trim_end()),
        None => (false, s),
    };
    let s = s.strip_prefix('$').unwrap_or(s);
    // Thousands separators only between digit groups of three.
    let cleaned: String = if s.contains(',') {
        let int_part = s.split(['.', 'e', 'E']).next().unwrap_or("");
        let groups: Vec<&str> = int_part.trim_start_matches(['+', '-']).split(',').collect();
        if groups.len() > 1 && (groups[0].is_empty() || groups[0].len() > 3 || groups[1..].iter().any(|g| g.len() != 3)) {
            return None;
        }
        s.replace(',', "")
    } else {
        s.to_string()
    };
    let bytes = cleaned.as_bytes();
    if bytes.is_empty() || !bytes.iter().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'+' | b'-' | b'e' | b'E')) {
        return None;
    }
    if !bytes.iter().any(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut v: f64 = cleaned.parse().ok()?;
    if neg {
        v = -v;
    }
    if pct {
        v /= 100.0;
    }
    v.is_finite().then_some(v)
}

/// Excel "General" conversion of a number to text (as used by `&`, TEXT/LEN of numbers).
/// Up to 15 significant digits; scientific notation outside the General range.
pub fn number_to_text(n: f64) -> String {
    if n == 0.0 {
        return "0".into();
    }
    let a = n.abs();
    // Round to 15 significant digits first.
    let sci = format!("{:.14e}", a);
    let (mant, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let digits: String = mant.replace('.', "");
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let sign = if n < 0.0 { "-" } else { "" };
    if (-10..21).contains(&exp) && !(exp >= 15) && !(exp < -9) {
        // Plain decimal.
        let mut s = String::new();
        if exp >= 0 {
            let int_len = (exp + 1) as usize;
            if digits.len() <= int_len {
                s.push_str(digits);
                s.extend(std::iter::repeat_n('0', int_len - digits.len()));
            } else {
                s.push_str(&digits[..int_len]);
                s.push('.');
                s.push_str(&digits[int_len..]);
            }
        } else {
            s.push_str("0.");
            s.extend(std::iter::repeat_n('0', (-exp - 1) as usize));
            s.push_str(digits);
        }
        format!("{sign}{s}")
    } else {
        let mut m = digits[..1].to_string();
        if digits.len() > 1 {
            m.push('.');
            m.push_str(&digits[1..]);
        }
        format!("{sign}{m}E{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

/// Excel comparison: numbers < text < logicals; text case-insensitive; Empty acts as the
/// other side's zero value. Errors must be handled by the caller.
pub fn compare(a: &Scalar, b: &Scalar) -> Ordering {
    fn rank(s: &Scalar) -> u8 {
        match s {
            Scalar::Num(_) | Scalar::Empty => 0,
            Scalar::Text(_) => 1,
            Scalar::Bool(_) => 2,
            Scalar::Err(_) => 3,
        }
    }
    match (a, b) {
        (Scalar::Empty, Scalar::Empty) => Ordering::Equal,
        (Scalar::Empty, Scalar::Text(t)) => cmp_text("", t),
        (Scalar::Text(t), Scalar::Empty) => cmp_text(t, ""),
        (Scalar::Empty, Scalar::Bool(x)) => false.cmp(x),
        (Scalar::Bool(x), Scalar::Empty) => x.cmp(&false),
        (Scalar::Empty, Scalar::Num(y)) => 0.0f64.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Scalar::Num(x), Scalar::Empty) => x.partial_cmp(&0.0).unwrap_or(Ordering::Equal),
        (Scalar::Num(x), Scalar::Num(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Scalar::Text(x), Scalar::Text(y)) => cmp_text(x, y),
        (Scalar::Bool(x), Scalar::Bool(y)) => x.cmp(y),
        _ => rank(a).cmp(&rank(b)),
    }
}

pub fn cmp_text(a: &str, b: &str) -> Ordering {
    if a.is_ascii() && b.is_ascii() {
        let (x, y) = (a.as_bytes(), b.as_bytes());
        for (p, q) in x.iter().zip(y) {
            let (p, q) = (p.to_ascii_lowercase(), q.to_ascii_lowercase());
            if p != q {
                return p.cmp(&q);
            }
        }
        return x.len().cmp(&y.len());
    }
    a.to_lowercase().cmp(&b.to_lowercase())
}

pub fn text_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.eq_ignore_ascii_case(b) || (!a.is_ascii() || !b.is_ascii()) && a.to_lowercase() == b.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn general_format() {
        for (n, s) in [
            (1.0, "1"),
            (-2.5, "-2.5"),
            (1.0 / 3.0, "0.333333333333333"),
            (123456789012.0, "123456789012"),
            (1e20, "1E+20"),
            (1234567890123456.0, "1.23456789012346E+15"),
            (0.1 + 0.2, "0.3"),
            (1e-10, "1E-10"),
            (0.0001, "0.0001"),
            (100.0, "100"),
        ] {
            assert_eq!(number_to_text(n), s, "{n}");
        }
    }
    #[test]
    fn coercion() {
        assert_eq!(parse_number(" 12 "), Some(12.0));
        assert_eq!(parse_number("1,000.5"), Some(1000.5));
        assert_eq!(parse_number("1e3"), Some(1000.0));
        assert_eq!(parse_number("50%"), Some(0.5));
        assert_eq!(parse_number("abc"), None);
        assert_eq!(parse_number("1,00"), None);
        assert_eq!(parse_number(""), None);
        assert_eq!(compare(&Scalar::Num(5.0), &Scalar::text("a")), Ordering::Less);
        assert_eq!(compare(&Scalar::text("ABC"), &Scalar::text("abc")), Ordering::Equal);
        assert_eq!(compare(&Scalar::Empty, &Scalar::text("")), Ordering::Equal);
    }
}
