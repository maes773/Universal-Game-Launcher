//! Lector JSON mínimo (solo lectura, sin dependencias) para las respuestas públicas de Steam Store.

use std::fmt;

const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonError(pub String);

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JSON no válido: {}", self.0)
    }
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(items) => Some(items.as_slice()),
            _ => None,
        }
    }

    /// Número entero no negativo (acepta también un texto con dígitos: Steam mezcla ambos).
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Num(n) if *n >= 0.0 && n.fract() == 0.0 => Some(*n as u64),
            Json::Str(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    pub fn entries(&self) -> &[(String, Json)] {
        match self {
            Json::Obj(entries) => entries.as_slice(),
            _ => &[],
        }
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

fn err<T>(message: &str) -> Result<T, JsonError> {
    Err(JsonError(message.to_string()))
}

impl<'a> Parser<'a> {
    fn skip_whitespace(&mut self) {
        while self.pos < self.bytes.len()
            && matches!(self.bytes[self.pos], b' ' | b'\t' | b'\n' | b'\r')
        {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn expect(&mut self, byte: u8) -> Result<(), JsonError> {
        if self.peek() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            err(&format!("se esperaba «{}»", byte as char))
        }
    }

    fn literal(&mut self, text: &str, value: Json) -> Result<Json, JsonError> {
        if self.bytes[self.pos..].starts_with(text.as_bytes()) {
            self.pos += text.len();
            Ok(value)
        } else {
            err("valor desconocido")
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, JsonError> {
        if depth > MAX_DEPTH {
            return err("anidamiento demasiado profundo");
        }
        self.skip_whitespace();
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(b'-') | Some(b'0'..=b'9') => self.number(),
            Some(_) => err("carácter inesperado"),
            None => err("el texto termina de forma inesperada"),
        }
    }

    fn number(&mut self) -> Result<Json, JsonError> {
        let start = self.pos;
        while self.pos < self.bytes.len()
            && matches!(self.bytes[self.pos], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
        {
            self.pos += 1;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| JsonError("número no válido".into()))?;
        text.parse::<f64>()
            .map(Json::Num)
            .map_err(|_| JsonError("número no válido".into()))
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let end = self.pos + 4;
        let digits = self
            .bytes
            .get(self.pos..end)
            .and_then(|b| std::str::from_utf8(b).ok())
            .ok_or_else(|| JsonError("secuencia \\u incompleta".into()))?;
        let value = u32::from_str_radix(digits, 16)
            .map_err(|_| JsonError("secuencia \\u no válida".into()))?;
        self.pos = end;
        Ok(value)
    }

    fn string(&mut self) -> Result<String, JsonError> {
        self.expect(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let byte = self
                .peek()
                .ok_or_else(|| JsonError("texto sin cerrar".into()))?;
            self.pos += 1;
            match byte {
                b'"' => break,
                b'\\' => {
                    let escape = self
                        .peek()
                        .ok_or_else(|| JsonError("texto sin cerrar".into()))?;
                    self.pos += 1;
                    let ch = match escape {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let first = self.hex4()?;
                            let code = if (0xD800..0xDC00).contains(&first)
                                && self.bytes[self.pos..].starts_with(b"\\u")
                            {
                                self.pos += 2;
                                let second = self.hex4()?;
                                0x10000 + ((first - 0xD800) << 10) + (second.wrapping_sub(0xDC00) & 0x3FF)
                            } else {
                                first
                            };
                            char::from_u32(code).unwrap_or('\u{FFFD}')
                        }
                        _ => return err("escape desconocido"),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                other => out.push(other),
            }
        }
        String::from_utf8(out).map_err(|_| JsonError("texto con UTF-8 no válido".into()))
    }

    fn array(&mut self, depth: usize) -> Result<Json, JsonError> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            items.push(self.value(depth + 1)?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Json::Arr(items));
                }
                _ => return err("se esperaba «,» o «]»"),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, JsonError> {
        self.expect(b'{')?;
        let mut entries = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Obj(entries));
        }
        loop {
            self.skip_whitespace();
            let key = self.string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            let value = self.value(depth + 1)?;
            entries.push((key, value));
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Json::Obj(entries));
                }
                _ => return err("se esperaba «,» o «}»"),
            }
        }
    }
}

pub fn parse(text: &str) -> Result<Json, JsonError> {
    let mut parser = Parser {
        bytes: text.trim_start_matches('\u{feff}').as_bytes(),
        pos: 0,
    };
    let value = parser.value(0)?;
    parser.skip_whitespace();
    if parser.pos != parser.bytes.len() {
        return err("hay texto después del valor");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_values_and_numbers() {
        let json = parse(r#" {"a": [1, 2.5, -3e2, "x"], "b": {"c": true, "d": null}} "#).unwrap();
        let a = json.get("a").and_then(Json::as_array).unwrap();
        assert_eq!(a[0].as_u64(), Some(1));
        assert_eq!(a[1], Json::Num(2.5));
        assert_eq!(a[2], Json::Num(-300.0));
        assert_eq!(a[3].as_str(), Some("x"));
        assert_eq!(json.get("b").and_then(|b| b.get("c")).and_then(Json::as_bool), Some(true));
        assert_eq!(json.get("b").and_then(|b| b.get("d")), Some(&Json::Null));
    }

    #[test]
    fn decodes_escapes_unicode_and_surrogate_pairs() {
        let json = parse(r#""Acci\u00f3n \"x\" \n\ud83c\udfae ñ""#).unwrap();
        assert_eq!(json.as_str(), Some("Acción \"x\" \n🎮 ñ"));
    }

    #[test]
    fn steam_style_ids_may_be_numbers_or_strings() {
        assert_eq!(parse("220").unwrap().as_u64(), Some(220));
        assert_eq!(parse("\"220\"").unwrap().as_u64(), Some(220));
        assert_eq!(parse("-1").unwrap().as_u64(), None);
        assert_eq!(parse("1.5").unwrap().as_u64(), None);
    }

    #[test]
    fn rejects_malformed_documents() {
        for bad in ["", "{", "[1,", "{\"a\" 1}", "tru", "\"abc", "{\"a\":1} x", "[1 2]", "<html>"] {
            assert!(parse(bad).is_err(), "debería fallar: {bad:?}");
        }
        let deep = "[".repeat(200);
        assert!(parse(&deep).is_err());
    }
}
