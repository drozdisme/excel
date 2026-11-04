//! Element-wise scalar functions: math, rounding, text and information functions.
use crate::scalar::{number_to_text, parse_number, Scalar};
use xlmodel::XlError;

type F = fn(&[Scalar]) -> Scalar;
type N1 = fn(f64) -> Result<f64, XlError>;

pub struct ScalarFn {
    pub min: usize,
    pub max: usize,
    defaults: &'static [(usize, Def)],
    pub propagate_errors: bool,
    pub num1: Option<N1>,
    pub func: F,
}

#[derive(Clone, Copy)]
enum Def {
    N(f64),
    B(bool),
    S(&'static str),
}

impl ScalarFn {
    pub fn default(&self, i: usize) -> Option<Scalar> {
        self.defaults.iter().find(|d| d.0 == i).map(|d| match d.1 {
            Def::N(n) => Scalar::Num(n),
            Def::B(b) => Scalar::Bool(b),
            Def::S(s) => Scalar::text(s),
        })
    }
}

macro_rules! num {
    ($s:expr) => {
        match $s.to_num() {
            Ok(v) => v,
            Err(e) => return Scalar::Err(e),
        }
    };
}
macro_rules! text {
    ($s:expr) => {
        match $s.to_text() {
            Ok(v) => v,
            Err(e) => return Scalar::Err(e),
        }
    };
}
macro_rules! boolean {
    ($s:expr) => {
        match $s.to_bool() {
            Ok(v) => v,
            Err(e) => return Scalar::Err(e),
        }
    };
}

fn n1(f: N1) -> F {
    // Never called: num1 is used instead; kept for the signature.
    let _ = f;
    |_| Scalar::Err(XlError::Value)
}

const fn sf(min: usize, max: usize, func: F) -> ScalarFn {
    ScalarFn { min, max, defaults: &[], propagate_errors: true, num1: None, func }
}

fn math(f: N1) -> ScalarFn {
    ScalarFn { min: 1, max: 1, defaults: &[], propagate_errors: true, num1: Some(f), func: n1(f) }
}

pub fn scalar_fn(name: &str) -> Option<ScalarFn> {
    let d = |defaults: &'static [(usize, Def)], f: ScalarFn| ScalarFn { defaults, ..f };
    Some(match name {
        "ABS" => math(|x| Ok(x.abs())),
        "SQRT" => math(|x| if x < 0.0 { Err(XlError::Num) } else { Ok(x.sqrt()) }),
        "SIN" => math(|x| Ok(x.sin())),
        "COS" => math(|x| Ok(x.cos())),
        "TAN" => math(|x| Ok(x.tan())),
        "ASIN" => math(|x| if x.abs() > 1.0 { Err(XlError::Num) } else { Ok(x.asin()) }),
        "ACOS" => math(|x| if x.abs() > 1.0 { Err(XlError::Num) } else { Ok(x.acos()) }),
        "ATAN" => math(|x| Ok(x.atan())),
        "SINH" => math(|x| Ok(x.sinh())),
        "COSH" => math(|x| Ok(x.cosh())),
        "TANH" => math(|x| Ok(x.tanh())),
        "RADIANS" => math(|x| Ok(x * std::f64::consts::PI / 180.0)),
        "DEGREES" => math(|x| Ok(x * 180.0 / std::f64::consts::PI)),
        "EXP" => math(|x| Ok(x.exp())),
        "LN" => math(|x| if x <= 0.0 { Err(XlError::Num) } else { Ok(x.ln()) }),
        "LOG10" => math(|x| if x <= 0.0 { Err(XlError::Num) } else { Ok(x.log10()) }),
        "INT" => math(|x| Ok(x.floor())),
        "SIGN" => math(|x| Ok(if x > 0.0 { 1.0 } else if x < 0.0 { -1.0 } else { 0.0 })),
        "EVEN" => math(|x| Ok(x.signum() * ((x.abs() / 2.0).ceil() * 2.0))),
        "ODD" => math(|x| {
            let a = x.abs().ceil();
            let a = if a % 2.0 == 0.0 { a + 1.0 } else { a };
            Ok(if x < 0.0 { -a } else { a })
        }),
        "FACT" => math(|x| {
            if x < 0.0 {
                return Err(XlError::Num);
            }
            Ok((1..=x.trunc() as u64).fold(1.0, |a, k| a * k as f64))
        }),
        "LOG" => d(&[(1, Def::N(10.0))], sf(1, 2, |a| {
            let (x, b) = (num!(a[0]), num!(a[1]));
            if x <= 0.0 || b <= 0.0 {
                return Scalar::Err(XlError::Num);
            }
            if b == 1.0 {
                return Scalar::Err(XlError::Div0);
            }
            Scalar::num(x.ln() / b.ln())
        })),
        "POWER" => sf(2, 2, |a| match crate::eval::power(num!(a[0]), num!(a[1])) {
            Ok(v) => Scalar::num(v),
            Err(e) => Scalar::Err(e),
        }),
        "ATAN2" => sf(2, 2, |a| {
            let (x, y) = (num!(a[0]), num!(a[1]));
            if x == 0.0 && y == 0.0 { Scalar::Err(XlError::Div0) } else { Scalar::num(y.atan2(x)) }
        }),
        "MOD" => sf(2, 2, |a| {
            let (x, y) = (num!(a[0]), num!(a[1]));
            if y == 0.0 {
                return Scalar::Err(XlError::Div0);
            }
            Scalar::num(x - y * (x / y).floor())
        }),
        "QUOTIENT" => sf(2, 2, |a| {
            let (x, y) = (num!(a[0]), num!(a[1]));
            if y == 0.0 { Scalar::Err(XlError::Div0) } else { Scalar::num((x / y).trunc()) }
        }),
        "ROUND" => sf(2, 2, |a| Scalar::num(round_excel(num!(a[0]), num!(a[1]), Rounding::Half))),
        "ROUNDUP" => sf(2, 2, |a| Scalar::num(round_excel(num!(a[0]), num!(a[1]), Rounding::Up))),
        "ROUNDDOWN" => sf(2, 2, |a| Scalar::num(round_excel(num!(a[0]), num!(a[1]), Rounding::Down))),
        "TRUNC" => d(&[(1, Def::N(0.0))], sf(1, 2, |a| Scalar::num(round_excel(num!(a[0]), num!(a[1]), Rounding::Down)))),
        "MROUND" => sf(2, 2, |a| {
            let (x, m) = (num!(a[0]), num!(a[1]));
            if m == 0.0 {
                return Scalar::Num(0.0);
            }
            if x * m < 0.0 {
                return Scalar::Err(XlError::Num);
            }
            Scalar::num(round_excel(x / m, 0.0, Rounding::Half) * m)
        }),
        "CEILING" | "CEILING.MATH" | "CEILING.PRECISE" => d(&[(1, Def::N(1.0))], sf(1, 3, |a| {
            let (x, s) = (num!(a[0]), num!(a[1]).abs());
            if s == 0.0 { Scalar::Num(0.0) } else { Scalar::num(round_sig((x / s).ceil_toward_inf()) * s) }
        })),
        "FLOOR" | "FLOOR.MATH" | "FLOOR.PRECISE" => d(&[(1, Def::N(1.0))], sf(1, 3, |a| {
            let (x, s) = (num!(a[0]), num!(a[1]).abs());
            if s == 0.0 { Scalar::Num(0.0) } else { Scalar::num(round_sig(x / s).floor() * s) }
        })),
        // ----------------------------------------------------------------- text
        "LOWER" => sf(1, 1, |a| Scalar::text(text!(a[0]).to_lowercase())),
        "UPPER" => sf(1, 1, |a| Scalar::text(text!(a[0]).to_uppercase())),
        "PROPER" => sf(1, 1, |a| {
            let mut out = String::new();
            let mut prev_letter = false;
            for ch in text!(a[0]).chars() {
                if ch.is_alphabetic() {
                    if prev_letter { out.extend(ch.to_lowercase()) } else { out.extend(ch.to_uppercase()) }
                    prev_letter = true;
                } else {
                    out.push(ch);
                    prev_letter = false;
                }
            }
            Scalar::text(out)
        }),
        "TRIM" => sf(1, 1, |a| Scalar::text(text!(a[0]).split(' ').filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" "))),
        "LEN" => sf(1, 1, |a| Scalar::Num(utf16_len(&text!(a[0])) as f64)),
        "LEFT" => d(&[(1, Def::N(1.0))], sf(1, 2, |a| {
            let (t, k) = (text!(a[0]), num!(a[1]));
            if k < 0.0 {
                return Scalar::Err(XlError::Value);
            }
            Scalar::text(t.chars().take(k as usize).collect::<String>())
        })),
        "RIGHT" => d(&[(1, Def::N(1.0))], sf(1, 2, |a| {
            let (t, k) = (text!(a[0]), num!(a[1]));
            if k < 0.0 {
                return Scalar::Err(XlError::Value);
            }
            let len = t.chars().count();
            Scalar::text(t.chars().skip(len.saturating_sub(k as usize)).collect::<String>())
        })),
        "MID" => sf(3, 3, |a| {
            let (t, s, k) = (text!(a[0]), num!(a[1]), num!(a[2]));
            if s < 1.0 || k < 0.0 {
                return Scalar::Err(XlError::Value);
            }
            Scalar::text(t.chars().skip(s as usize - 1).take(k as usize).collect::<String>())
        }),
        "FIND" => d(&[(2, Def::N(1.0))], sf(2, 3, find_impl::<false>)),
        "SEARCH" => d(&[(2, Def::N(1.0))], sf(2, 3, find_impl::<true>)),
        "SUBSTITUTE" => sf(3, 4, |a| {
            let (t, old, new) = (text!(a[0]), text!(a[1]), text!(a[2]));
            if old.is_empty() {
                return Scalar::Text(t);
            }
            if a.len() == 4 {
                let k = num!(a[3]);
                if k < 1.0 {
                    return Scalar::Err(XlError::Value);
                }
                match t.match_indices(&*old).nth(k as usize - 1) {
                    Some((pos, _)) => Scalar::text(format!("{}{}{}", &t[..pos], new, &t[pos + old.len()..])),
                    None => Scalar::Text(t),
                }
            } else {
                Scalar::text(t.replace(&*old, &new))
            }
        }),
        "REPT" => sf(2, 2, |a| {
            let (t, k) = (text!(a[0]), num!(a[1]));
            if k < 0.0 || t.len() as f64 * k > 32767.0 {
                return Scalar::Err(XlError::Value);
            }
            Scalar::text(t.repeat(k as usize))
        }),
        "EXACT" => sf(2, 2, |a| Scalar::Bool(text!(a[0]) == text!(a[1]))),
        "CONCATENATE" => sf(1, 255, |a| {
            let mut s = String::new();
            for x in a {
                s.push_str(&text!(x));
            }
            Scalar::text(s)
        }),
        "CHAR" => sf(1, 1, |a| {
            let k = num!(a[0]).trunc();
            if !(1.0..=255.0).contains(&k) {
                return Scalar::Err(XlError::Value);
            }
            Scalar::text(cp1252(k as u8).to_string())
        }),
        "UNICHAR" => sf(1, 1, |a| {
            let k = num!(a[0]).trunc();
            match (k >= 1.0).then(|| char::from_u32(k as u32)).flatten() {
                Some(c) => Scalar::text(c.to_string()),
                None => Scalar::Err(XlError::Value),
            }
        }),
        "CODE" | "UNICODE" => sf(1, 1, |a| match text!(a[0]).chars().next() {
            Some(c) => Scalar::Num(c as u32 as f64),
            None => Scalar::Err(XlError::Value),
        }),
        "VALUE" => sf(1, 1, |a| match &a[0] {
            Scalar::Num(n) => Scalar::Num(*n),
            Scalar::Empty => Scalar::Num(0.0),
            Scalar::Bool(_) => Scalar::Err(XlError::Value),
            s => match parse_number(&text!(s)) {
                Some(v) => Scalar::Num(v),
                None => Scalar::Err(XlError::Value),
            },
        }),
        "NUMBERVALUE" => d(&[(1, Def::S(".")), (2, Def::S(","))], sf(1, 3, |a| {
            let (t, dec, grp) = (text!(a[0]), text!(a[1]), text!(a[2]));
            let t: String = t.chars().filter(|c| !c.is_whitespace()).collect();
            if t.is_empty() {
                return Scalar::Num(0.0);
            }
            let dc = dec.chars().next().unwrap_or('.');
            let gc = grp.chars().next();
            let Some(dpos) = Some(t.find(dc)) else { unreachable!() };
            let mut s = String::new();
            for (i, ch) in t.char_indices() {
                if Some(ch) == gc && dpos.is_none_or(|d| i < d) {
                    continue;
                }
                s.push(if ch == dc { '.' } else { ch });
            }
            match parse_number(&s) {
                Some(v) => Scalar::Num(v),
                None => Scalar::Err(XlError::Value),
            }
        })),
        "FIXED" => d(&[(1, Def::N(2.0)), (2, Def::B(false))], sf(1, 3, |a| {
            let (x, dec, no_commas) = (num!(a[0]), num!(a[1]).trunc(), boolean!(a[2]));
            if dec > 127.0 {
                return Scalar::Err(XlError::Value);
            }
            Scalar::text(fixed(x, dec as i32, !no_commas))
        })),
        "TEXT" => sf(2, 2, |a| {
            let fmt = text!(a[1]);
            match crate::format::format_value(&a[0], &fmt) {
                Ok(s) => Scalar::text(s),
                Err(e) => Scalar::Err(e),
            }
        }),
        // ----------------------------------------------------------------- information
        "ISNUMBER" => info(|a| Scalar::Bool(matches!(a[0], Scalar::Num(_)))),
        "ISTEXT" => info(|a| Scalar::Bool(matches!(a[0], Scalar::Text(_)))),
        "ISNONTEXT" => info(|a| Scalar::Bool(!matches!(a[0], Scalar::Text(_)))),
        "ISBLANK" => info(|a| Scalar::Bool(matches!(a[0], Scalar::Empty))),
        "ISLOGICAL" => info(|a| Scalar::Bool(matches!(a[0], Scalar::Bool(_)))),
        "ISERROR" => info(|a| Scalar::Bool(a[0].is_err())),
        "ISERR" => info(|a| Scalar::Bool(matches!(a[0], Scalar::Err(e) if e != XlError::NA))),
        "ISNA" => info(|a| Scalar::Bool(matches!(a[0], Scalar::Err(XlError::NA)))),
        "ISEVEN" => sf(1, 1, |a| Scalar::Bool((num!(a[0]).trunc() as i64) % 2 == 0)),
        "ISODD" => sf(1, 1, |a| Scalar::Bool((num!(a[0]).trunc() as i64) % 2 != 0)),
        "NOT" => sf(1, 1, |a| Scalar::Bool(!boolean!(a[0]))),
        "N" => ScalarFn { propagate_errors: false, ..sf(1, 1, |a| match &a[0] {
            Scalar::Num(n) => Scalar::Num(*n),
            Scalar::Bool(b) => Scalar::Num(*b as u8 as f64),
            Scalar::Err(e) => Scalar::Err(*e),
            _ => Scalar::Num(0.0),
        }) },
        "T" => sf(1, 1, |a| match &a[0] {
            Scalar::Text(t) => Scalar::Text(t.clone()),
            _ => Scalar::text(""),
        }),
        "ERROR.TYPE" => ScalarFn { propagate_errors: false, ..sf(1, 1, |a| match &a[0] {
            Scalar::Err(e) => Scalar::Num(e.code() as f64),
            _ => Scalar::Err(XlError::NA),
        }) },
        _ => return None,
    })
}

/// Information functions see errors instead of propagating them.
fn info(func: F) -> ScalarFn {
    ScalarFn { min: 1, max: 1, defaults: &[], propagate_errors: false, num1: None, func }
}

fn find_impl<const CI: bool>(a: &[Scalar]) -> Scalar {
    let (needle, hay) = (text!(a[0]), text!(a[1]));
    let start = if a.len() > 2 { num!(a[2]) } else { 1.0 };
    let hay_chars: Vec<char> = hay.chars().collect();
    if start < 1.0 || start as usize > hay_chars.len() + 1 {
        return Scalar::Err(XlError::Value);
    }
    let from = start as usize - 1;
    if needle.is_empty() {
        return Scalar::Num(start.trunc());
    }
    let nchars: Vec<char> = needle.chars().collect();
    for i in from..hay_chars.len() {
        let rest: String = hay_chars[i..].iter().collect();
        let hit = if CI {
            if crate::lookup::has_wildcard(&needle) {
                // SEARCH wildcards match a prefix of the remaining text.
                (i..=hay_chars.len()).any(|j| crate::lookup::wildcard(&needle, &hay_chars[i..j].iter().collect::<String>()))
            } else {
                rest.to_lowercase().starts_with(&needle.to_lowercase())
            }
        } else {
            hay_chars[i..].starts_with(&nchars)
        };
        if hit {
            return Scalar::Num((i + 1) as f64);
        }
    }
    Scalar::Err(XlError::Value)
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn cp1252(b: u8) -> char {
    const HIGH: [u16; 32] = [
        0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018,
        0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
    ];
    if (0x80..0xA0).contains(&b) { char::from_u32(HIGH[(b - 0x80) as usize] as u32).unwrap_or('?') } else { b as char }
}

trait CeilInf {
    fn ceil_toward_inf(self) -> f64;
}
impl CeilInf for f64 {
    fn ceil_toward_inf(self) -> f64 {
        round_sig(self).ceil()
    }
}

/// Round to 15 significant digits, as Excel does before rounding/truncation decisions.
pub fn round_sig(x: f64) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    format!("{:.14e}", x).parse().unwrap_or(x)
}

#[derive(Clone, Copy)]
pub enum Rounding {
    Half,
    Up,
    Down,
}

pub fn round_excel(x: f64, digits: f64, mode: Rounding) -> f64 {
    let d = digits.trunc() as i32;
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    let d = d.clamp(-308, 308);
    let p = 10f64.powi(d.abs());
    let scaled = if d >= 0 { x * p } else { x / p };
    let s = round_sig(scaled);
    let r = match mode {
        Rounding::Half => s.abs().add_half_floor() * s.signum(),
        Rounding::Up => s.abs().ceil() * s.signum(),
        Rounding::Down => s.abs().floor() * s.signum(),
    };
    let out = if d >= 0 { r / p } else { r * p };
    round_sig(out)
}

trait HalfFloor {
    fn add_half_floor(self) -> f64;
}
impl HalfFloor for f64 {
    fn add_half_floor(self) -> f64 {
        (self + 0.5).floor()
    }
}

/// FIXED: round to `dec` decimals and format with optional thousands separators.
pub fn fixed(x: f64, dec: i32, commas: bool) -> String {
    let r = round_excel(x, dec as f64, Rounding::Half);
    let body = if dec > 0 { format!("{:.*}", dec as usize, r.abs()) } else { format!("{:.0}", r.abs()) };
    let (int, frac) = match body.split_once('.') {
        Some((i, f)) => (i.to_string(), Some(f.to_string())),
        None => (body.clone(), None),
    };
    let int = if commas { group3(&int) } else { int };
    let neg = r < 0.0 && body.chars().any(|c| c.is_ascii_digit() && c != '0');
    format!("{}{}{}", if neg { "-" } else { "" }, int, frac.map(|f| format!(".{f}")).unwrap_or_default())
}

pub fn group3(int: &str) -> String {
    let b = int.as_bytes();
    let mut out = String::with_capacity(int.len() + int.len() / 3);
    for (i, ch) in b.iter().enumerate() {
        if i > 0 && (b.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*ch as char);
    }
    out
}

#[allow(dead_code)]
fn general(n: f64) -> String {
    number_to_text(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rounding() {
        assert_eq!(round_excel(2.675, 2.0, Rounding::Half), 2.68);
        assert_eq!(round_excel(-2.5, 0.0, Rounding::Half), -3.0);
        assert_eq!(round_excel(1234.5678, -2.0, Rounding::Half), 1200.0);
        assert_eq!(round_excel(0.285, 2.0, Rounding::Up), 0.29);
        assert_eq!(round_excel(0.3 - 0.1, 1.0, Rounding::Down), 0.2);
        assert_eq!(round_excel(-1.234, 1.0, Rounding::Down), -1.2);
        assert_eq!(fixed(1234567.891, 2, true), "1,234,567.89");
        assert_eq!(fixed(-0.004, 2, true), "0.00");
        assert_eq!(fixed(1234.5, 0, false), "1235");
        assert_eq!(fixed(-1234.5, -2, true), "-1,200");
    }
    #[test]
    fn text_fns() {
        let f = scalar_fn("SEARCH").unwrap();
        assert_eq!((f.func)(&[Scalar::text("b*d"), Scalar::text("abcde")]), Scalar::Num(2.0));
        let f = scalar_fn("FIND").unwrap();
        assert_eq!((f.func)(&[Scalar::text("B"), Scalar::text("abcB")]), Scalar::Num(4.0));
        let f = scalar_fn("ISNUMBER").unwrap();
        assert_eq!((f.func)(&[Scalar::Num(1.0)]), Scalar::Bool(true));
        assert_eq!((f.func)(&[Scalar::text("1")]), Scalar::Bool(false));
        let f = scalar_fn("MID").unwrap();
        assert_eq!((f.func)(&[Scalar::text("abcdef"), Scalar::Num(2.0), Scalar::Num(3.0)]), Scalar::text("bcd"));
    }
}
