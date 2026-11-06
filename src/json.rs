// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A small strict JSON reader for the vendor's session log (one record per line).
//!
//! Strict on purpose: a torn or foreign line is an error, which the log reader turns into
//! "unreadable" (fail closed), never into "quiet". Bounded nesting, no allocation beyond
//! the value.

/// A JSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `null`.
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// A non-negative integer that fits `u64`.
    UInt(u64),
    /// Any other number.
    Float(f64),
    /// A string.
    Str(String),
    /// An array.
    Array(Vec<Value>),
    /// An object, members in order.
    Object(Vec<(String, Value)>),
}

impl Value {
    /// The member `key` of an object (the last one, as most readers take a duplicate).
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(members) => members.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The string, if this is one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The integer, if this is a non-negative one that fits `u64`.
    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::UInt(n) => Some(*n),
            _ => None,
        }
    }
}

/// Why a text is not one JSON value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error;

/// How deep arrays and objects may nest.
const MAX_DEPTH: usize = 128;

/// Parse `text` as exactly one JSON value (surrounding whitespace allowed).
///
/// # Errors
/// [`Error`] for anything else.
pub fn parse(text: &str) -> Result<Value, Error> {
    let mut p = Parser {
        b: text.as_bytes(),
        i: 0,
    };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i == p.b.len() { Ok(v) } else { Err(Error) }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    /// Step past the byte [`Parser::peek`] returned. Every caller steps only past a byte
    /// it peeked, so the cursor is below the end here; the check states that where the
    /// step is, so the cursor can never pass the end of the text (nor overflow).
    fn bump(&mut self) {
        if self.i < self.b.len() {
            self.i += 1;
        }
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.bump();
        }
    }

    /// Step past `lit`, byte by byte, or fail. A failure leaves the cursor anywhere inside
    /// `lit`: every caller propagates it, so the whole parse fails.
    fn eat(&mut self, lit: &[u8]) -> Result<(), Error> {
        for &c in lit {
            if self.peek() != Some(c) {
                return Err(Error);
            }
            self.bump();
        }
        Ok(())
    }

    fn value(&mut self, depth: usize) -> Result<Value, Error> {
        if depth > MAX_DEPTH {
            return Err(Error);
        }
        match self.peek().ok_or(Error)? {
            b'n' => self.eat(b"null").map(|()| Value::Null),
            b't' => self.eat(b"true").map(|()| Value::Bool(true)),
            b'f' => self.eat(b"false").map(|()| Value::Bool(false)),
            b'"' => self.string().map(Value::Str),
            b'[' => {
                self.bump();
                let mut items = Vec::new();
                self.ws();
                if self.peek() == Some(b']') {
                    self.bump();
                    return Ok(Value::Array(items));
                }
                loop {
                    self.ws();
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.bump(),
                        Some(b']') => {
                            self.bump();
                            return Ok(Value::Array(items));
                        }
                        _ => return Err(Error),
                    }
                }
            }
            b'{' => {
                self.bump();
                let mut members = Vec::new();
                self.ws();
                if self.peek() == Some(b'}') {
                    self.bump();
                    return Ok(Value::Object(members));
                }
                loop {
                    self.ws();
                    if self.peek() != Some(b'"') {
                        return Err(Error);
                    }
                    let key = self.string()?;
                    self.ws();
                    self.eat(b":")?;
                    self.ws();
                    let v = self.value(depth + 1)?;
                    members.push((key, v));
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.bump(),
                        Some(b'}') => {
                            self.bump();
                            return Ok(Value::Object(members));
                        }
                        _ => return Err(Error),
                    }
                }
            }
            b'-' | b'0'..=b'9' => self.number(),
            _ => Err(Error),
        }
    }

    fn number(&mut self) -> Result<Value, Error> {
        let start = self.i;
        let negative = self.peek() == Some(b'-');
        if negative {
            self.bump();
        }
        match self.peek() {
            Some(b'0') => self.bump(),
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.bump();
                }
            }
            _ => return Err(Error),
        }
        let mut integral = !negative;
        if self.peek() == Some(b'.') {
            integral = false;
            self.bump();
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(Error);
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.bump();
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            integral = false;
            self.bump();
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.bump();
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(Error);
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.bump();
            }
        }
        let digits = self.b.get(start..self.i).ok_or(Error)?;
        let text = std::str::from_utf8(digits).map_err(|_| Error)?;
        if integral && let Ok(n) = text.parse::<u64>() {
            return Ok(Value::UInt(n));
        }
        text.parse::<f64>().map(Value::Float).map_err(|_| Error)
    }

    /// Four hex digits (either case), as the code unit they spell.
    fn hex4(&mut self) -> Result<u32, Error> {
        let mut unit = 0u32;
        for _ in 0..4 {
            let digit = match self.peek() {
                Some(c @ b'0'..=b'9') => c - b'0',
                Some(c @ b'a'..=b'f') => c - b'a' + 10,
                Some(c @ b'A'..=b'F') => c - b'A' + 10,
                _ => return Err(Error),
            };
            self.bump();
            unit = (unit << 4) | u32::from(digit);
        }
        Ok(unit)
    }

    fn string(&mut self) -> Result<String, Error> {
        self.eat(b"\"")?;
        let mut out = String::new();
        loop {
            let start = self.i;
            while let Some(c) = self.peek() {
                if c == b'"' || c == b'\\' || c < 0x20 {
                    break;
                }
                self.bump();
            }
            let run = self.b.get(start..self.i).ok_or(Error)?;
            out.push_str(std::str::from_utf8(run).map_err(|_| Error)?);
            match self.peek().ok_or(Error)? {
                b'"' => {
                    self.bump();
                    return Ok(out);
                }
                b'\\' => {
                    self.bump();
                    let esc = self.peek().ok_or(Error)?;
                    self.bump();
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let code = match hi {
                                0xD800..=0xDBFF => {
                                    self.eat(b"\\u")?;
                                    match self.hex4()? {
                                        lo @ 0xDC00..=0xDFFF => {
                                            0x10000 + (hi - 0xD800) * 0x400 + (lo - 0xDC00)
                                        }
                                        _ => return Err(Error),
                                    }
                                }
                                _ => hi,
                            };
                            out.push(char::from_u32(code).ok_or(Error)?);
                        }
                        _ => return Err(Error),
                    }
                }
                _ => return Err(Error),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_record_parses() {
        let line = r#"{"schema_version":1,"recorded_at":1792284625000001,"payload_type":"runtime.session.route_facts","payload":{"kind":"route_facts","record":{"cwd":"/w","pid":57495,"ok":true,"none":null,"list":[1,-2,3.5e2]}}}"#;
        let v = parse(line).expect("parses");
        assert_eq!(
            v.get("recorded_at").and_then(Value::as_u64),
            Some(1_792_284_625_000_001)
        );
        assert_eq!(
            v.get("payload_type").and_then(Value::as_str),
            Some("runtime.session.route_facts")
        );
        let rec = v
            .get("payload")
            .and_then(|p| p.get("record"))
            .expect("record");
        assert_eq!(rec.get("pid").and_then(Value::as_u64), Some(57_495));
        assert_eq!(rec.get("ok"), Some(&Value::Bool(true)));
        assert_eq!(
            rec.get("list"),
            Some(&Value::Array(vec![
                Value::UInt(1),
                Value::Float(-2.0),
                Value::Float(350.0)
            ]))
        );
    }

    #[test]
    fn escapes_decode_and_torn_text_fails() {
        assert_eq!(
            parse(r#""a\"b\\c\né😀""#),
            Ok(Value::Str("a\"b\\c\né😀".to_string()))
        );
        for bad in [
            "",
            "{",
            r#"{"a":1"#,
            r#"{"schema_version":1,"payload_type":"runtime.sess"#,
            "[1,]",
            "01",
            "1.",
            "nul",
            r#""\ud83d""#,
            "{\"a\":1} x",
            "\"tab\there\"",
        ] {
            assert_eq!(parse(bad), Err(Error), "{bad:?}");
        }
        let deep = "[".repeat(MAX_DEPTH + 2) + &"]".repeat(MAX_DEPTH + 2);
        assert_eq!(parse(&deep), Err(Error), "nesting is bounded");
    }

    #[test]
    fn unicode_escapes_decode_and_bad_ones_fail() {
        assert_eq!(
            parse(r#""\u00e9\u00E9\ud83d\ude00\u0041""#),
            Ok(Value::Str("éé😀A".to_string()))
        );
        assert_eq!(
            parse(r#""\uDBFF\uDFFF""#),
            Ok(Value::Str("\u{10FFFF}".to_string()))
        );
        for bad in [
            r#""\ud83d\u0041""#,
            r#""\ude00""#,
            r#""\u12""#,
            r#""\u12g4""#,
            r#""\ud83d\""#,
            r#""\ud83dx""#,
        ] {
            assert_eq!(parse(bad), Err(Error), "{bad:?}");
        }
    }
}
