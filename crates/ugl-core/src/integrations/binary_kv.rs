//! Lector de KeyValues binario de Valve (el formato de `UserGameStats*.bin`).
//! Solo lectura. Tolerante con bytes sobrantes al final del archivo.

use thiserror::Error;

const MAX_DEPTH: usize = 32;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BkvError {
    #[error("el archivo termina de forma inesperada")]
    UnexpectedEof,
    #[error("tipo de valor desconocido: 0x{0:02x}")]
    UnknownType(u8),
    #[error("anidamiento demasiado profundo")]
    TooDeep,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Bkv {
    Str(String),
    Int(i64),
    Float(f32),
    Obj(Vec<(String, Bkv)>),
}

impl Bkv {
    /// Busca una clave sin distinguir mayúsculas.
    pub fn get(&self, key: &str) -> Option<&Bkv> {
        match self {
            Bkv::Obj(entries) => entries
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn entries(&self) -> &[(String, Bkv)] {
        match self {
            Bkv::Obj(entries) => entries.as_slice(),
            _ => &[],
        }
    }

    pub fn as_text(&self) -> Option<String> {
        match self {
            Bkv::Str(s) => Some(s.clone()),
            Bkv::Int(i) => Some(i.to_string()),
            Bkv::Float(f) => Some(f.to_string()),
            Bkv::Obj(_) => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Bkv::Int(i) => Some(*i),
            Bkv::Str(s) => s.trim().parse().ok(),
            Bkv::Float(f) => Some(*f as i64),
            Bkv::Obj(_) => None,
        }
    }

    pub fn truthy(&self) -> bool {
        match self {
            Bkv::Int(i) => *i != 0,
            Bkv::Str(s) => {
                let text = s.trim();
                !text.is_empty() && text != "0"
            }
            Bkv::Float(f) => *f != 0.0,
            Bkv::Obj(_) => false,
        }
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Result<u8, BkvError> {
        let byte = *self.data.get(self.pos).ok_or(BkvError::UnexpectedEof)?;
        self.pos += 1;
        Ok(byte)
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], BkvError> {
        let end = self.pos.checked_add(count).ok_or(BkvError::UnexpectedEof)?;
        let slice = self
            .data
            .get(self.pos..end)
            .ok_or(BkvError::UnexpectedEof)?;
        self.pos = end;
        Ok(slice)
    }

    /// Texto terminado en cero (UTF-8; los bytes no válidos se sustituyen).
    fn cstring(&mut self) -> Result<String, BkvError> {
        let rest = &self.data[self.pos..];
        let len = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or(BkvError::UnexpectedEof)?;
        let text = String::from_utf8_lossy(&rest[..len]).into_owned();
        self.pos += len + 1;
        Ok(text)
    }

    fn wide_string(&mut self) -> Result<String, BkvError> {
        let mut units = Vec::new();
        loop {
            let pair = self.take(2)?;
            let unit = u16::from_le_bytes([pair[0], pair[1]]);
            if unit == 0 {
                break;
            }
            units.push(unit);
        }
        Ok(String::from_utf16_lossy(&units))
    }

    fn i32_le(&mut self) -> Result<i32, BkvError> {
        let mut buf = [0u8; 4];
        buf.copy_from_slice(self.take(4)?);
        Ok(i32::from_le_bytes(buf))
    }

    fn eight(&mut self) -> Result<[u8; 8], BkvError> {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(self.take(8)?);
        Ok(buf)
    }
}

fn parse_object(reader: &mut Reader<'_>, depth: usize) -> Result<Vec<(String, Bkv)>, BkvError> {
    let mut entries = Vec::new();
    loop {
        if reader.pos >= reader.data.len() {
            // Un archivo puede terminar sin el último marcador de cierre en el nivel raíz.
            return if depth == 0 {
                Ok(entries)
            } else {
                Err(BkvError::UnexpectedEof)
            };
        }
        let kind = reader.u8()?;
        if kind == 0x08 {
            return Ok(entries);
        }
        let key = reader.cstring()?;
        let value = match kind {
            0x00 => {
                if depth >= MAX_DEPTH {
                    return Err(BkvError::TooDeep);
                }
                Bkv::Obj(parse_object(reader, depth + 1)?)
            }
            0x01 => Bkv::Str(reader.cstring()?),
            0x02 | 0x04 | 0x06 => Bkv::Int(reader.i32_le()? as i64),
            0x03 => Bkv::Float(f32::from_bits(reader.i32_le()? as u32)),
            0x05 => Bkv::Str(reader.wide_string()?),
            0x07 => Bkv::Int(u64::from_le_bytes(reader.eight()?) as i64),
            0x0A => Bkv::Int(i64::from_le_bytes(reader.eight()?)),
            other => return Err(BkvError::UnknownType(other)),
        };
        entries.push((key, value));
    }
}

/// Interpreta un documento completo. El resultado es un objeto raíz implícito.
pub fn parse(data: &[u8]) -> Result<Bkv, BkvError> {
    let mut reader = Reader { data, pos: 0 };
    Ok(Bkv::Obj(parse_object(&mut reader, 0)?))
}

/// Constructores de bytes para pruebas (los archivos reales no se pueden generar aquí).
#[cfg(test)]
pub(crate) mod test_util {
    pub fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    pub fn obj(key: &str, body: Vec<u8>) -> Vec<u8> {
        let mut out = vec![0x00];
        out.extend_from_slice(key.as_bytes());
        out.push(0);
        out.extend(body);
        out.push(0x08);
        out
    }

    pub fn st(key: &str, value: &str) -> Vec<u8> {
        let mut out = vec![0x01];
        out.extend_from_slice(key.as_bytes());
        out.push(0);
        out.extend_from_slice(value.as_bytes());
        out.push(0);
        out
    }

    pub fn int(key: &str, value: i32) -> Vec<u8> {
        let mut out = vec![0x02];
        out.extend_from_slice(key.as_bytes());
        out.push(0);
        out.extend_from_slice(&value.to_le_bytes());
        out
    }

    /// Documento completo: objeto raíz + marcador de cierre final.
    pub fn document(root: Vec<u8>) -> Vec<u8> {
        let mut out = root;
        out.push(0x08);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::*;
    use super::*;

    #[test]
    fn parses_nested_values() {
        let bytes = document(obj(
            "root",
            cat(&[
                st("texto", "hola"),
                int("numero", -1),
                obj("hijo", int("x", 7)),
            ]),
        ));
        let doc = parse(&bytes).unwrap();
        let root = doc.get("root").unwrap();
        assert_eq!(root.get("texto").and_then(Bkv::as_text), Some("hola".to_string()));
        assert_eq!(root.get("numero").and_then(Bkv::as_i64), Some(-1));
        assert_eq!(
            root.get("hijo").and_then(|h| h.get("x")).and_then(Bkv::as_i64),
            Some(7)
        );
    }

    #[test]
    fn reports_truncated_and_unknown_data() {
        // Objeto anidado sin cerrar.
        let truncated = obj("a", st("k", "v"));
        let cut = &truncated[..truncated.len() - 3];
        assert!(parse(cut).is_err());
        assert_eq!(parse(&[0x42, b'k', 0, 0]), Err(BkvError::UnknownType(0x42)));
    }

    #[test]
    fn rejects_absurd_nesting() {
        let mut bytes = Vec::new();
        for _ in 0..100 {
            bytes.extend_from_slice(&[0x00, b'k', 0]);
        }
        assert_eq!(parse(&bytes), Err(BkvError::TooDeep));
    }
}
