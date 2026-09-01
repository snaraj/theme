//! A minimal JSON reader for the two API answers this tool parses (Unsplash
//! photo objects and the OAuth token response). In-house on purpose: the
//! dependency budget is zero here, the input shapes are small, and every
//! consumer treats missing/mismatched fields as an ordinary miss rather than
//! an error — exactly how the shell's python readers behaved.

use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(HashMap<String, Json>),
}

impl Json {
    pub fn parse(s: &str) -> Option<Json> {
        let b = s.as_bytes();
        let mut i = 0usize;
        let v = value(b, &mut i)?;
        skip_ws(b, &mut i);
        if i == b.len() { Some(v) } else { None }
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(m) => m.get(key),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    /// `get(key)` as a non-empty string.
    pub fn str_field(&self, key: &str) -> Option<&str> {
        self.get(key)
            .and_then(Json::as_str)
            .filter(|s| !s.is_empty())
    }
}

fn skip_ws(b: &[u8], i: &mut usize) {
    while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\n' | b'\r') {
        *i += 1;
    }
}

fn value(b: &[u8], i: &mut usize) -> Option<Json> {
    skip_ws(b, i);
    match b.get(*i)? {
        b'{' => obj(b, i),
        b'[' => arr(b, i),
        b'"' => string(b, i).map(Json::Str),
        b't' => lit(b, i, b"true", Json::Bool(true)),
        b'f' => lit(b, i, b"false", Json::Bool(false)),
        b'n' => lit(b, i, b"null", Json::Null),
        _ => num(b, i),
    }
}

fn lit(b: &[u8], i: &mut usize, word: &[u8], v: Json) -> Option<Json> {
    if b[*i..].starts_with(word) {
        *i += word.len();
        Some(v)
    } else {
        None
    }
}

fn num(b: &[u8], i: &mut usize) -> Option<Json> {
    let start = *i;
    if b.get(*i) == Some(&b'-') {
        *i += 1;
    }
    while *i < b.len()
        && (b[*i].is_ascii_digit() || matches!(b[*i], b'.' | b'e' | b'E' | b'+' | b'-'))
    {
        *i += 1;
    }
    std::str::from_utf8(&b[start..*i])
        .ok()?
        .parse()
        .ok()
        .map(Json::Num)
}

fn string(b: &[u8], i: &mut usize) -> Option<String> {
    if b.get(*i) != Some(&b'"') {
        return None;
    }
    *i += 1;
    let mut out = String::new();
    loop {
        match b.get(*i)? {
            b'"' => {
                *i += 1;
                return Some(out);
            }
            b'\\' => {
                *i += 1;
                match b.get(*i)? {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{8}'),
                    b'f' => out.push('\u{c}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        let hex = std::str::from_utf8(b.get(*i + 1..*i + 5)?).ok()?;
                        let cp = u32::from_str_radix(hex, 16).ok()?;
                        *i += 4;
                        // Surrogate pairs: a high surrogate must be followed
                        // by \uDC00-\uDFFF; anything else renders U+FFFD.
                        if (0xD800..0xDC00).contains(&cp) {
                            if b.get(*i + 1..*i + 3) == Some(b"\\u") {
                                let hex2 = std::str::from_utf8(b.get(*i + 3..*i + 7)?).ok()?;
                                let lo = u32::from_str_radix(hex2, 16).ok()?;
                                if (0xDC00..0xE000).contains(&lo) {
                                    *i += 6;
                                    let c = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                    out.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
                                } else {
                                    out.push('\u{FFFD}');
                                }
                            } else {
                                out.push('\u{FFFD}');
                            }
                        } else {
                            out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                        }
                    }
                    _ => return None,
                }
                *i += 1;
            }
            _ => {
                // Consume one UTF-8 scalar, byte-faithfully.
                let s = std::str::from_utf8(&b[*i..]).ok()?;
                let c = s.chars().next()?;
                out.push(c);
                *i += c.len_utf8();
            }
        }
    }
}

fn arr(b: &[u8], i: &mut usize) -> Option<Json> {
    *i += 1;
    let mut out = Vec::new();
    skip_ws(b, i);
    if b.get(*i) == Some(&b']') {
        *i += 1;
        return Some(Json::Arr(out));
    }
    loop {
        out.push(value(b, i)?);
        skip_ws(b, i);
        match b.get(*i)? {
            b',' => *i += 1,
            b']' => {
                *i += 1;
                return Some(Json::Arr(out));
            }
            _ => return None,
        }
    }
}

fn obj(b: &[u8], i: &mut usize) -> Option<Json> {
    *i += 1;
    let mut out = HashMap::new();
    skip_ws(b, i);
    if b.get(*i) == Some(&b'}') {
        *i += 1;
        return Some(Json::Obj(out));
    }
    loop {
        skip_ws(b, i);
        let k = string(b, i)?;
        skip_ws(b, i);
        if b.get(*i) != Some(&b':') {
            return None;
        }
        *i += 1;
        out.insert(k, value(b, i)?);
        skip_ws(b, i);
        match b.get(*i)? {
            b',' => *i += 1,
            b'}' => {
                *i += 1;
                return Some(Json::Obj(out));
            }
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_unsplash_shape() {
        let j = Json::parse(
            r#"{"id":"x","width":3840,"urls":{"raw":"https://a/b"},"user":{"name":"Ann é"},"tags":[1,-2.5e1,null,true]}"#,
        )
        .unwrap();
        assert_eq!(j.get("width").unwrap().as_f64(), Some(3840.0));
        assert_eq!(j.get("urls").unwrap().str_field("raw"), Some("https://a/b"));
        assert_eq!(j.get("user").unwrap().str_field("name"), Some("Ann é"));
        assert!(matches!(j.get("tags").unwrap(), Json::Arr(a) if a.len() == 4));
    }

    #[test]
    fn hostile_input_is_a_miss_not_a_panic() {
        for s in [
            "{",
            "]",
            "\"\\u12",
            "{\"a\":}",
            "{\"a\":1}trailing",
            "\u{0}",
        ] {
            assert!(Json::parse(s).is_none(), "{s:?} parsed");
        }
    }

    #[test]
    fn surrogate_pairs_and_escapes() {
        let j = Json::parse(r#""a😀b\tc""#).unwrap();
        assert_eq!(j.as_str(), Some("a😀b\tc"));
        // A lone high surrogate degrades to U+FFFD rather than failing.
        let j = Json::parse(r#""x\ud83dy""#).unwrap();
        assert_eq!(j.as_str(), Some("x\u{FFFD}y"));
    }
}
