//! Number format codes for TEXT(): sections, 0/#/? digits, thousands, %, scientific,
//! literals and '@'. Date/time codes are rejected explicitly (fail-closed).
use crate::scalar::{number_to_text, Scalar};
use crate::scalars::{group3, round_excel, Rounding};
use xlmodel::XlError;

pub fn format_value(v: &Scalar, fmt: &str) -> Result<String, XlError> {
    match v {
        Scalar::Err(e) => Err(*e),
        Scalar::Text(t) => {
            // Text uses the 4th section (or '@' in the first) else passes through.
            let sections = split_sections(fmt);
            let sec = sections.get(3).or_else(|| sections.first().filter(|s| s.contains('@')));
            Ok(match sec {
                Some(s) => s.replace('@', t).replace('"', ""),
                None => t.to_string(),
            })
        }
        Scalar::Bool(b) => Ok(if *b { "TRUE" } else { "FALSE" }.into()),
        Scalar::Empty => format_number(0.0, fmt),
        Scalar::Num(n) => format_number(*n, fmt),
    }
}

fn split_sections(fmt: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut quoted = false;
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                out.last_mut().unwrap().push(c);
            }
            '\\' if !quoted => {
                out.last_mut().unwrap().push(c);
                if let Some(n) = chars.next() {
                    out.last_mut().unwrap().push(n);
                }
            }
            ';' if !quoted => out.push(String::new()),
            _ => out.last_mut().unwrap().push(c),
        }
    }
    out
}

pub fn format_number(n: f64, fmt: &str) -> Result<String, XlError> {
    if fmt.is_empty() {
        return Ok(String::new());
    }
    if fmt.eq_ignore_ascii_case("general") {
        return Ok(number_to_text(n));
    }
    let sections = split_sections(fmt);
    let (sec, negate_sign) = match (sections.len(), n) {
        (1, _) => (&sections[0], false),
        (_, x) if x < 0.0 => (&sections[1], true),
        (s, x) if x == 0.0 && s >= 3 => (&sections[2], false),
        _ => (&sections[0], false),
    };
    let value = if negate_sign { n.abs() } else { n };
    let mut s = render(value, sec)?;
    if n < 0.0 && !negate_sign && s.chars().any(|c| c.is_ascii_digit() && c != '0') {
        s.insert(0, '-');
    }
    Ok(s)
}

fn render(n: f64, sec: &str) -> Result<String, XlError> {
    if sec.eq_ignore_ascii_case("general") {
        return Ok(number_to_text(n.abs()));
    }
    // Tokenise into literals and the numeric pattern.
    let lower = sec.to_ascii_lowercase();
    if lower.chars().any(|c| matches!(c, 'y' | 'm' | 'd' | 'h' | 's')) && !sec.contains('"') {
        return Err(XlError::Value); // dates/times not supported
    }
    let mut prefix = String::new();
    let mut suffix = String::new();
    let mut pattern = String::new();
    let mut in_pattern_done = false;
    let mut chars = sec.chars().peekable();
    let mut quoted = false;
    let mut percent = 0;
    while let Some(c) = chars.next() {
        let target = if pattern.is_empty() { &mut prefix } else if in_pattern_done { &mut suffix } else { &mut suffix };
        match c {
            '"' => quoted = !quoted,
            _ if quoted => target.push(c),
            '\\' => {
                if let Some(x) = chars.next() {
                    target.push(x)
                }
            }
            '0' | '#' | '?' | '.' | ',' | 'E' | 'e' | '+' | '-' if !in_pattern_done && (c != '-' && c != '+' || pattern.ends_with(['E', 'e'])) => {
                if !suffix.is_empty() {
                    in_pattern_done = true;
                    suffix.push(c);
                } else {
                    pattern.push(c)
                }
            }
            '%' => {
                percent += 1;
                if pattern.is_empty() { prefix.push('%') } else { suffix.push('%') }
            }
            '_' => {
                chars.next();
                target.push(' ');
            }
            '*' => {
                chars.next();
            }
            '[' => {
                for x in chars.by_ref() {
                    if x == ']' {
                        break;
                    }
                }
            }
            c => target.push(c),
        }
    }
    let v = n * 100f64.powi(percent);
    if pattern.is_empty() {
        return Ok(format!("{prefix}{suffix}"));
    }
    let body = if let Some(epos) = pattern.find(['E', 'e']) {
        let (mant, exp) = pattern.split_at(epos);
        let dec = mant.split('.').nth(1).map(|f| f.len()).unwrap_or(0);
        let exp_digits = exp.trim_start_matches(['E', 'e', '+', '-']).len().max(1);
        if v == 0.0 {
            format!("{:.*}E+{:0w$}", dec, 0.0, 0, w = exp_digits)
        } else {
            let e = v.abs().log10().floor() as i32;
            let m = round_excel(v.abs() / 10f64.powi(e), dec as f64, Rounding::Half);
            let (m, e) = if m >= 10.0 { (m / 10.0, e + 1) } else { (m, e) };
            format!("{:.*}E{}{:0w$}", dec, m, if e < 0 { '-' } else { '+' }, e.abs(), w = exp_digits)
        }
    } else {
        // Trailing commas scale by 1000 each.
        let trimmed = pattern.trim_end_matches(',');
        let scale = pattern.len() - trimmed.len();
        let v = v.abs() / 1000f64.powi(scale as i32);
        let (int_pat, frac_pat) = match trimmed.split_once('.') {
            Some((i, f)) => (i.to_string(), f.to_string()),
            None => (trimmed.to_string(), String::new()),
        };
        let thousands = int_pat.contains(',');
        let int_pat: String = int_pat.chars().filter(|c| *c != ',').collect();
        let dec = frac_pat.len();
        let r = round_excel(v, dec as f64, Rounding::Half);
        let s = format!("{:.*}", dec, r);
        let (mut ip, mut fp) = match s.split_once('.') {
            Some((i, f)) => (i.to_string(), f.to_string()),
            None => (s.clone(), String::new()),
        };
        let min_int = int_pat.chars().filter(|c| *c == '0').count();
        if ip == "0" && min_int == 0 {
            ip.clear();
        }
        while ip.len() < min_int {
            ip.insert(0, '0');
        }
        if thousands {
            ip = group3(&ip);
        }
        // Optional fraction digits ('#') drop trailing zeros.
        let required = frac_pat.chars().filter(|c| *c == '0').count();
        while fp.len() > required && fp.ends_with('0') {
            fp.pop();
        }
        if fp.is_empty() && !frac_pat.is_empty() {
            format!("{ip}.")
        } else if fp.is_empty() {
            ip
        } else {
            format!("{ip}.{fp}")
        }
    };
    Ok(format!("{prefix}{body}{suffix}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn formats() {
        let f = |n: f64, s: &str| format_number(n, s).unwrap();
        assert_eq!(f(3.14159, "0.00"), "3.14");
        assert_eq!(f(-3.14159, "0.000"), "-3.142");
        assert_eq!(f(1234567.0, "#,##0"), "1,234,567");
        assert_eq!(f(0.256, "0.0%"), "25.6%");
        assert_eq!(f(12345.0, "0.00E+00"), "1.23E+04");
        assert_eq!(f(7.0, "000"), "007");
        assert_eq!(f(-5.0, "0;(0)"), "(5)");
        assert_eq!(f(0.5, "#.##"), ".5");
        assert_eq!(f(2.0, "0.##"), "2.");
    }
}
