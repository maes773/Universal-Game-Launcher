//! Lector mínimo del formato de texto VDF/ACF de Valve (KeyValues).
//! Solo lectura; suficiente para `appmanifest_*.acf` y `libraryfolders.vdf`.

use thiserror::Error;

const MAX_DEPTH: usize = 32;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VdfError {
    #[error("texto entre comillas sin cerrar")]
    UnterminatedString,
    #[error("el archivo termina de forma inesperada")]
    UnexpectedEof,
    #[error("símbolo inesperado: {0}")]
    UnexpectedToken(String),
    #[error("anidamiento demasiado profundo")]
    TooDeep,
    #[error("falta la sección «{0}»")]
    Missing(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Vdf {
    Str(String),
    Obj(Vec<(String, Vdf)>),
}

impl Vdf {
    /// Busca una clave sin distinguir mayúsculas (Steam no es consistente entre archivos).
    pub fn get(&self, key: &str) -> Option<&Vdf> {
        match self {
            Vdf::Obj(entries) => entries
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v),
            Vdf::Str(_) => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Vdf::Str(s) => Some(s.as_str()),
            Vdf::Obj(_) => None,
        }
    }

    pub fn entries(&self) -> &[(String, Vdf)] {
        match self {
            Vdf::Obj(entries) => entries.as_slice(),
            Vdf::Str(_) => &[],
        }
    }
}

enum Token {
    Str(String),
    Open,
    Close,
}

struct Lexer<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> Lexer<'a> {
    fn next_token(&mut self) -> Result<Option<Token>, VdfError> {
        loop {
            match self.chars.peek().copied() {
                None => return Ok(None),
                Some(c) if c.is_whitespace() || c == '\u{feff}' => {
                    self.chars.next();
                }
                Some('/') => {
                    self.chars.next();
                    if self.chars.peek() == Some(&'/') {
                        // Comentario hasta el final de la línea.
                        for c in self.chars.by_ref() {
                            if c == '\n' {
                                break;
                            }
                        }
                    } else {
                        return Err(VdfError::UnexpectedToken("/".into()));
                    }
                }
                Some('{') => {
                    self.chars.next();
                    return Ok(Some(Token::Open));
                }
                Some('}') => {
                    self.chars.next();
                    return Ok(Some(Token::Close));
                }
                Some('"') => {
                    self.chars.next();
                    return self.read_quoted().map(|s| Some(Token::Str(s)));
                }
                Some(_) => {
                    // Palabra sin comillas (poco habitual, pero válida).
                    let mut word = String::new();
                    while let Some(&c) = self.chars.peek() {
                        if c.is_whitespace() || c == '{' || c == '}' || c == '"' {
                            break;
                        }
                        word.push(c);
                        self.chars.next();
                    }
                    return Ok(Some(Token::Str(word)));
                }
            }
        }
    }

    fn read_quoted(&mut self) -> Result<String, VdfError> {
        let mut out = String::new();
        loop {
            match self.chars.next() {
                None => return Err(VdfError::UnterminatedString),
                Some('"') => return Ok(out),
                Some('\\') => match self.chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('\\') => out.push('\\'),
                    Some('"') => out.push('"'),
                    // Rutas antiguas con una sola barra invertida: se conservan tal cual.
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => return Err(VdfError::UnterminatedString),
                },
                Some(c) => out.push(c),
            }
        }
    }
}

fn parse_entries(lexer: &mut Lexer<'_>, depth: usize) -> Result<Vec<(String, Vdf)>, VdfError> {
    if depth > MAX_DEPTH {
        return Err(VdfError::TooDeep);
    }
    let nested = depth > 0;
    let mut entries = Vec::new();
    loop {
        match lexer.next_token()? {
            None => {
                return if nested {
                    Err(VdfError::UnexpectedEof)
                } else {
                    Ok(entries)
                };
            }
            Some(Token::Close) => {
                return if nested {
                    Ok(entries)
                } else {
                    Err(VdfError::UnexpectedToken("}".into()))
                };
            }
            Some(Token::Open) => return Err(VdfError::UnexpectedToken("{".into())),
            Some(Token::Str(key)) => match lexer.next_token()? {
                Some(Token::Str(value)) => entries.push((key, Vdf::Str(value))),
                Some(Token::Open) => {
                    let children = parse_entries(lexer, depth + 1)?;
                    entries.push((key, Vdf::Obj(children)));
                }
                Some(Token::Close) => return Err(VdfError::UnexpectedToken("}".into())),
                None => return Err(VdfError::UnexpectedEof),
            },
        }
    }
}

/// Interpreta un documento VDF completo. El resultado es un objeto raíz implícito:
/// `parse(texto)?.get("AppState")`.
pub fn parse(input: &str) -> Result<Vdf, VdfError> {
    let mut lexer = Lexer {
        chars: input.chars().peekable(),
    };
    Ok(Vdf::Obj(parse_entries(&mut lexer, 0)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_objects_and_escapes() {
        let text = "\"root\"\n{\n\t\"path\"\t\"D:\\\\Steam\\\\Lib\"\n\t\"inner\"\n\t{\n\t\t\"k\" \"v\"\n\t}\n}\n";
        let doc = parse(text).unwrap();
        let root = doc.get("ROOT").unwrap();
        assert_eq!(root.get("path").and_then(Vdf::as_str), Some("D:\\Steam\\Lib"));
        assert_eq!(
            root.get("inner").and_then(|i| i.get("k")).and_then(Vdf::as_str),
            Some("v")
        );
    }

    #[test]
    fn ignores_comments_and_bom() {
        let text = "\u{feff}// comentario\n\"a\" \"1\" // otro\n\"b\" \"2\"\n";
        let doc = parse(text).unwrap();
        assert_eq!(doc.get("a").and_then(Vdf::as_str), Some("1"));
        assert_eq!(doc.get("b").and_then(Vdf::as_str), Some("2"));
    }

    #[test]
    fn reports_malformed_input() {
        assert_eq!(parse("\"abc"), Err(VdfError::UnterminatedString));
        assert_eq!(parse("\"a\" {"), Err(VdfError::UnexpectedEof));
        assert!(matches!(parse("}"), Err(VdfError::UnexpectedToken(_))));
    }

    #[test]
    fn rejects_absurd_nesting() {
        let mut text = String::new();
        for _ in 0..100 {
            text.push_str("\"k\" {");
        }
        assert_eq!(parse(&text), Err(VdfError::TooDeep));
    }
}
