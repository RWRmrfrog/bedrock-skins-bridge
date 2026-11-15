//! A small JSON reader/writer for Bedrock geometry files.
//!
//! Object key order and number spelling are preserved, so geometry passes through unchanged
//! apart from whitespace.

use std::fmt::Write as _;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    /// The number as written in the source.
    Num(String),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

const MAX_DEPTH: usize = 256;

impl Json {
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim_start_matches('\u{feff}');
        let mut p = Parser {
            s: text.as_bytes(),
            i: 0,
        };
        p.ws();
        let v = p.value(0)?;
        p.ws();
        // Bedrock files sometimes carry trailing NULs.
        while p.i < p.s.len() && (p.s[p.i] == 0 || p.s[p.i].is_ascii_whitespace()) {
            p.i += 1;
        }
        if p.i != p.s.len() {
            return Err(format!("trailing data at byte {}", p.i));
        }
        Ok(v)
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        if let Self::Str(s) = self { Some(s) } else { None }
    }

    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Num(n) => n
                .parse::<i64>()
                .ok()
                .or_else(|| n.parse::<f64>().ok().map(|f| f as i64)),
            _ => None,
        }
    }

    #[must_use]
    pub fn to_compact(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Self::Num(n) => out.push_str(n),
            Self::Str(s) => write_str(s, out),
            Self::Arr(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Self::Obj(fields) => {
                out.push('{');
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_str(k, out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

fn write_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn err<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("{what} at byte {}", self.i))
    }

    fn eat(&mut self, lit: &str) -> bool {
        if self.s[self.i..].starts_with(lit.as_bytes()) {
            self.i += lit.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAX_DEPTH {
            return self.err("nesting too deep");
        }
        let Some(&c) = self.s.get(self.i) else {
            return self.err("unexpected end");
        };
        match c {
            b'{' => {
                self.i += 1;
                let mut fields = Vec::new();
                self.ws();
                if self.eat("}") {
                    return Ok(Json::Obj(fields));
                }
                loop {
                    self.ws();
                    if self.s.get(self.i) != Some(&b'"') {
                        return self.err("expected key");
                    }
                    let k = self.string()?;
                    self.ws();
                    if !self.eat(":") {
                        return self.err("expected ':'");
                    }
                    self.ws();
                    let v = self.value(depth + 1)?;
                    fields.push((k, v));
                    self.ws();
                    if self.eat(",") {
                        // Tolerate a trailing comma, which some skin packs contain.
                        self.ws();
                        if self.eat("}") {
                            return Ok(Json::Obj(fields));
                        }
                        continue;
                    }
                    if self.eat("}") {
                        return Ok(Json::Obj(fields));
                    }
                    return self.err("expected ',' or '}'");
                }
            }
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.eat("]") {
                    return Ok(Json::Arr(items));
                }
                loop {
                    self.ws();
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    if self.eat(",") {
                        self.ws();
                        if self.eat("]") {
                            return Ok(Json::Arr(items));
                        }
                        continue;
                    }
                    if self.eat("]") {
                        return Ok(Json::Arr(items));
                    }
                    return self.err("expected ',' or ']'");
                }
            }
            b'"' => Ok(Json::Str(self.string()?)),
            b't' if self.eat("true") => Ok(Json::Bool(true)),
            b'f' if self.eat("false") => Ok(Json::Bool(false)),
            b'n' if self.eat("null") => Ok(Json::Null),
            b'-' | b'0'..=b'9' => {
                let start = self.i;
                self.i += 1;
                while self.i < self.s.len() && matches!(self.s[self.i], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
                {
                    self.i += 1;
                }
                let n = std::str::from_utf8(&self.s[start..self.i]).expect("ascii");
                if n.parse::<f64>().is_err() {
                    return self.err("bad number");
                }
                Ok(Json::Num(n.to_owned()))
            }
            _ => self.err("unexpected character"),
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let Some(h) = self.s.get(self.i..self.i + 4) else {
            return self.err("bad escape");
        };
        let h = std::str::from_utf8(h).map_err(|_| "bad escape".to_owned())?;
        let v = u32::from_str_radix(h, 16).map_err(|_| format!("bad escape at byte {}", self.i))?;
        self.i += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1; // opening quote
        let mut out = Vec::new();
        loop {
            let Some(&c) = self.s.get(self.i) else {
                return self.err("unterminated string");
            };
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).map_err(|_| "invalid UTF-8 in string".to_owned()),
                b'\\' => {
                    let Some(&e) = self.s.get(self.i) else {
                        return self.err("bad escape");
                    };
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            let cp = if (0xD800..0xDC00).contains(&hi) && self.eat("\\u") {
                                let lo = self.hex4()?;
                                if (0xDC00..0xE000).contains(&lo) {
                                    0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                                } else {
                                    0xFFFD
                                }
                            } else {
                                hi
                            };
                            char::from_u32(cp).unwrap_or('\u{fffd}')
                        }
                        _ => return self.err("bad escape"),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                c => out.push(c),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_preserves_order_and_numbers() {
        let src = r#"{ "z": 1.50, "a": [true, false, null, -0.25e2], "s": "q\"\\é😀", "o": {} }"#;
        let v = Json::parse(src).unwrap();
        assert_eq!(
            v.to_compact(),
            "{\"z\":1.50,\"a\":[true,false,null,-0.25e2],\"s\":\"q\\\"\\\\é😀\",\"o\":{}}"
        );
        assert_eq!(Json::parse(&v.to_compact()).unwrap(), v);
    }

    #[test]
    fn tolerates_bom_trailing_commas_and_nuls() {
        let v = Json::parse("\u{feff}{\"a\":[1,2,],}\0\0").unwrap();
        assert_eq!(
            v.get("a").unwrap(),
            &Json::Arr(vec![Json::Num("1".into()), Json::Num("2".into())])
        );
    }

    #[test]
    fn rejects_bad_input() {
        for bad in ["", "{", "[1 2]", "{\"a\" 1}", "\"abc", "nul", "{} x", "-", "1e"] {
            assert!(Json::parse(bad).is_err(), "{bad}");
        }
        let deep = "[".repeat(1000);
        assert!(Json::parse(&deep).is_err());
    }
}
