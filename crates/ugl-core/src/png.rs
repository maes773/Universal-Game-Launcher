//! Codificador PNG mínimo (RGBA de 8 bits, sin compresión). Suficiente para la caché de iconos:
//! evita una dependencia solo para guardar imágenes de 64×64.

/// CRC-32 (el de PNG/zlib). Valor de prueba: «123456789» → 0xCBF43926.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Adler-32 (el de zlib). Valor de prueba: «Wikipedia» → 0x11E60398.
pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + byte as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

/// Flujo zlib con bloques «almacenados» (sin comprimir).
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    if data.is_empty() {
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    let mut blocks = data.chunks(65_535).peekable();
    while let Some(block) = blocks.next() {
        let last = blocks.peek().is_none();
        out.push(if last { 1 } else { 0 });
        let len = block.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut body = Vec::with_capacity(4 + data.len());
    body.extend_from_slice(kind);
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

/// Codifica píxeles RGBA (sin premultiplicar). Devuelve `None` si las dimensiones no cuadran.
pub fn encode_rgba(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let row = (width as usize).checked_mul(4)?;
    let expected = row.checked_mul(height as usize)?;
    if width == 0 || height == 0 || rgba.len() != expected {
        return None;
    }

    let mut raw = Vec::with_capacity((row + 1) * height as usize);
    for line in rgba.chunks_exact(row) {
        raw.push(0); // filtro «ninguno»
        raw.extend_from_slice(line);
    }

    let mut out = Vec::new();
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]); // 8 bits, RGBA, deflate, filtro 0, sin entrelazado
    push_chunk(&mut out, b"IHDR", &header);
    push_chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    push_chunk(&mut out, b"IEND", &[]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_match_known_vectors() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn encodes_a_valid_looking_png() {
        let pixels = vec![255u8; 4 * 3 * 2];
        let png = encode_rgba(3, 2, &pixels).unwrap();
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[16..20], &3u32.to_be_bytes());
        assert_eq!(&png[20..24], &2u32.to_be_bytes());
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
    }

    #[test]
    fn rejects_bad_dimensions() {
        assert!(encode_rgba(0, 1, &[]).is_none());
        assert!(encode_rgba(2, 2, &[0u8; 15]).is_none());
    }

    #[test]
    fn large_images_use_several_stored_blocks() {
        // 200×200×4 + 200 filtros = 160 200 bytes → 3 bloques de hasta 65 535.
        let pixels = vec![7u8; 200 * 200 * 4];
        assert!(encode_rgba(200, 200, &pixels).is_some());
    }
}
