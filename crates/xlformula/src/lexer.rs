use crate::ParseError;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Number(f64),
    Text(String),
    /// Identifier-like word: cell ref, column, name, function, TRUE/FALSE.
    Word(String),
    /// Row number range component like `$3` or `12` when used in `3:5`; lexed as Word too.
    Error(String),
    /// Sheet prefix ending with '!': quoted or plain sheet name.
    Sheet(String),
    /// External workbook prefix like `[1]`.
    External(String),
    LParen,
    RParen,
    LBrace,
    RBrace,
    Comma,
    Semicolon,
    Colon,
    Op(&'static str),
    Hash,
    At,
    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub space_before: bool,
    pub pos: usize,
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '.' | '\\' | '?' | '$')
}

pub fn lex(src: &str) -> Result<Vec<Token>, ParseError> {
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut space = false;
    let err = |pos: usize, msg: &str| ParseError { pos, msg: msg.to_string() };
    while i < chars.len() {
        let (pos, c) = chars[i];
        if c.is_whitespace() {
            space = true;
            i += 1;
            continue;
        }
        let push = |out: &mut Vec<Token>, tok: Tok, space: &mut bool| {
            out.push(Token { tok, space_before: *space, pos });
            *space = false;
        };
        match c {
            '"' => {
                let mut s = String::new();
                i += 1;
                loop {
                    let Some(&(_, ch)) = chars.get(i) else { return Err(err(pos, "unterminated string")) };
                    i += 1;
                    if ch == '"' {
                        if chars.get(i).map(|x| x.1) == Some('"') {
                            s.push('"');
                            i += 1;
                        } else {
                            break;
                        }
                    } else {
                        s.push(ch);
                    }
                }
                push(&mut out, Tok::Text(s), &mut space);
            }
            '\'' => {
                let mut s = String::new();
                i += 1;
                loop {
                    let Some(&(_, ch)) = chars.get(i) else { return Err(err(pos, "unterminated sheet name")) };
                    i += 1;
                    if ch == '\'' {
                        if chars.get(i).map(|x| x.1) == Some('\'') {
                            s.push('\'');
                            i += 1;
                        } else {
                            break;
                        }
                    } else {
                        s.push(ch);
                    }
                }
                if chars.get(i).map(|x| x.1) != Some('!') {
                    return Err(err(pos, "quoted name must be a sheet prefix"));
                }
                i += 1;
                let (ext, sheet) = split_external(&s);
                if let Some(e) = ext {
                    push(&mut out, Tok::External(e), &mut space);
                }
                push(&mut out, Tok::Sheet(sheet), &mut space);
            }
            '[' => {
                let start = i;
                while i < chars.len() && chars[i].1 != ']' {
                    i += 1;
                }
                i += 1;
                let s: String = chars[start..i.min(chars.len())].iter().map(|x| x.1).collect();
                push(&mut out, Tok::External(s), &mut space);
            }
            '#' => {
                // Error literal or spill operator.
                const ERRS: [&str; 16] = [
                    "#NULL!", "#DIV/0!", "#VALUE!", "#REF!", "#NAME?", "#NUM!", "#N/A", "#GETTING_DATA", "#SPILL!", "#CALC!",
                    "#FIELD!", "#CONNECT!", "#BLOCKED!", "#UNKNOWN!", "#BUSY!", "#PYTHON!",
                ];
                let rest = &src[pos..];
                if let Some(e) = ERRS.iter().find(|e| rest.len() >= e.len() && rest[..e.len()].eq_ignore_ascii_case(e)) {
                    i += e.chars().count();
                    push(&mut out, Tok::Error(e.to_string()), &mut space);
                } else {
                    i += 1;
                    push(&mut out, Tok::Hash, &mut space);
                }
            }
            '0'..='9' | '.' if c != '.' || chars.get(i + 1).is_some_and(|x| x.1.is_ascii_digit()) => {
                // Number, unless it is part of a word like a row range "3:5" (handled by parser) or
                // a word starting with digits (not valid Excel anyway).
                let start = i;
                while i < chars.len() && (chars[i].1.is_ascii_digit() || chars[i].1 == '.') {
                    i += 1;
                }
                if i < chars.len() && matches!(chars[i].1, 'e' | 'E') {
                    let save = i;
                    i += 1;
                    if i < chars.len() && matches!(chars[i].1, '+' | '-') {
                        i += 1;
                    }
                    if i < chars.len() && chars[i].1.is_ascii_digit() {
                        while i < chars.len() && chars[i].1.is_ascii_digit() {
                            i += 1;
                        }
                    } else {
                        i = save;
                    }
                }
                let text: String = chars[start..i].iter().map(|x| x.1).collect();
                // A plain-digit word followed by '!' would be a sheet name like 2019!A1.
                if i < chars.len() && chars[i].1 == '!' {
                    i += 1;
                    push(&mut out, Tok::Sheet(text), &mut space);
                } else {
                    let v: f64 = text.parse().map_err(|_| err(pos, "bad number"))?;
                    push(&mut out, Tok::Number(v), &mut space);
                }
            }
            c if is_word_char(c) => {
                let start = i;
                while i < chars.len() && is_word_char(chars[i].1) {
                    i += 1;
                }
                let w: String = chars[start..i].iter().map(|x| x.1).collect();
                if i < chars.len() && chars[i].1 == '!' {
                    i += 1;
                    push(&mut out, Tok::Sheet(w), &mut space);
                } else if i < chars.len() && chars[i].1 == '[' && !w.starts_with('$') {
                    // Structured reference Table[...]: not supported, keep as error-producing word.
                    return Err(err(pos, "structured table references are not supported"));
                } else {
                    push(&mut out, Tok::Word(w), &mut space);
                }
            }
            '(' => {
                i += 1;
                push(&mut out, Tok::LParen, &mut space)
            }
            ')' => {
                i += 1;
                push(&mut out, Tok::RParen, &mut space)
            }
            '{' => {
                i += 1;
                push(&mut out, Tok::LBrace, &mut space)
            }
            '}' => {
                i += 1;
                push(&mut out, Tok::RBrace, &mut space)
            }
            ',' => {
                i += 1;
                push(&mut out, Tok::Comma, &mut space)
            }
            ';' => {
                i += 1;
                push(&mut out, Tok::Semicolon, &mut space)
            }
            ':' => {
                i += 1;
                push(&mut out, Tok::Colon, &mut space)
            }
            '@' => {
                i += 1;
                push(&mut out, Tok::At, &mut space)
            }
            '<' | '>' => {
                let two = chars.get(i + 1).map(|x| x.1);
                let op = match (c, two) {
                    ('<', Some('=')) => "<=",
                    ('<', Some('>')) => "<>",
                    ('>', Some('=')) => ">=",
                    ('<', _) => "<",
                    _ => ">",
                };
                i += op.len();
                push(&mut out, Tok::Op(op), &mut space);
            }
            '+' | '-' | '*' | '/' | '^' | '&' | '=' | '%' => {
                let op = match c {
                    '+' => "+",
                    '-' => "-",
                    '*' => "*",
                    '/' => "/",
                    '^' => "^",
                    '&' => "&",
                    '=' => "=",
                    _ => "%",
                };
                i += 1;
                push(&mut out, Tok::Op(op), &mut space);
            }
            _ => return Err(err(pos, &format!("unexpected character '{c}'"))),
        }
    }
    out.push(Token { tok: Tok::Eof, space_before: space, pos: src.len() });
    Ok(out)
}

fn split_external(s: &str) -> (Option<String>, String) {
    if let Some(rest) = s.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            return (Some(format!("[{}]", &rest[..end])), rest[end + 1..].to_string());
        }
    }
    (None, s.to_string())
}
