//! Hashes a cell's source is named by, as `jupyter-server-documents` does.

/// MurmurHash2 (32-bit) of `data`, as `jupyter-server-documents` hashes
/// a cell's UTF-8 source (seed 0) for its execute endpoint, so a request
/// names the source it saw and a changed cell is refused.
pub fn murmur2(data: &[u8], seed: u32) -> u32 {
    const M: u32 = 0x5BD1_E995;
    let mut h = seed ^ data.len() as u32;
    let (chunks, rest) = data.as_chunks::<4>();
    for c in chunks {
        let mut k = u32::from_le_bytes(*c);
        k = k.wrapping_mul(M);
        k ^= k >> 24;
        k = k.wrapping_mul(M);
        h = h.wrapping_mul(M) ^ k;
    }
    if rest.len() == 3 {
        h ^= u32::from(rest[2]) << 16;
    }
    if rest.len() >= 2 {
        h ^= u32::from(rest[1]) << 8;
    }
    if !rest.is_empty() {
        h ^= u32::from(rest[0]);
        h = h.wrapping_mul(M);
    }
    h ^= h >> 13;
    h = h.wrapping_mul(M);
    h ^= h >> 15;
    h
}

/// The hash a request names a cell's source by: MurmurHash2, seed 0, as
/// a decimal string.
pub fn source_hash(source: &str) -> String {
    murmur2(source.as_bytes(), 0).to_string()
}

#[cfg(test)]
mod tests {
    /// Values from `jupyter_server_documents.rooms.ynotebook_room._source_hash`.
    #[test]
    fn murmur2_matches_jupyter_server_documents() {
        for (s, h) in [
            ("", 0),
            ("a", 2456313694),
            ("ab", 446775395),
            ("abc", 324500635),
            ("abcd", 646393889),
            ("print('hello')\n", 1612567987),
            ("héllo wörld 🚀", 1432791399),
        ] {
            assert_eq!(super::murmur2(s.as_bytes(), 0), h, "{s:?}");
        }
    }

    #[test]
    fn source_hash_is_the_hash_as_a_decimal_string() {
        assert_eq!(
            super::source_hash("print('hello')\n"),
            super::murmur2(b"print('hello')\n", 0).to_string()
        );
    }
}
