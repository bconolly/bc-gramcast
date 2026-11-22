//! Small pure encoders: hex, standard base64 and percent-encoding. No I/O, no
//! dependencies. Decoders are strict: anything not canonical is `None`.

const HEX: &[u8; 16] = b"0123456789abcdef";
const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Lower-case hex, two characters per byte.
pub fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[usize::from(b >> 4)] as char);
        out.push(HEX[usize::from(b & 15)] as char);
    }
    out
}

/// True when `text` is exactly `len` lower-case hex characters.
pub fn is_lower_hex(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Standard base64 (RFC 4648 section 4) with padding.
pub fn b64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn b64_value(b: u8) -> Option<u32> {
    B64.iter().position(|c| *c == b).map(|p| p as u32)
}

/// Strict standard base64: padded, only the standard alphabet, and the unused
/// trailing bits must be zero (so each value has one encoding). At most
/// `max_len` characters are accepted.
pub fn b64_decode(text: &str, max_len: usize) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if bytes.len() > max_len || !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let groups = bytes.len() / 4;
    for (g, chunk) in bytes.chunks(4).enumerate() {
        let pad = chunk.iter().rev().take_while(|b| **b == b'=').count();
        if pad > 2 || (pad > 0 && g + 1 != groups) {
            return None;
        }
        let mut n = 0u32;
        for b in &chunk[..4 - pad] {
            n = (n << 6) | b64_value(*b)?;
        }
        n <<= 6 * pad as u32;
        let produced = 3 - pad;
        // Bits beyond the produced bytes must be zero.
        if n & ((1u32 << (8 * (3 - produced))) - 1) != 0 {
            return None;
        }
        for i in 0..produced {
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    Some(out)
}

/// Percent-encodes every byte except the RFC 3986 unreserved set
/// (letters, digits, `-`, `.`, `_`, `~`), so the result is safe in a query value.
pub fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[usize::from(b >> 4)].to_ascii_uppercase() as char);
            out.push(HEX[usize::from(b & 15)].to_ascii_uppercase() as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_encodes_lower_case() {
        assert_eq!(hex_encode(&[]), "");
        assert_eq!(hex_encode(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
    }

    #[test]
    fn lower_hex_check_is_exact() {
        assert!(is_lower_hex("00ff", 4));
        assert!(!is_lower_hex("00FF", 4));
        assert!(!is_lower_hex("00f", 4));
        assert!(!is_lower_hex("00fg", 4));
        assert!(!is_lower_hex("00ff00", 4));
    }

    #[test]
    fn base64_matches_the_rfc_4648_vectors() {
        for (plain, coded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(b64_encode(plain.as_bytes()), coded);
            assert_eq!(b64_decode(coded, 64), Some(plain.as_bytes().to_vec()));
        }
    }

    #[test]
    fn base64_uses_the_standard_alphabet() {
        assert_eq!(b64_encode(&[0xfb, 0xff, 0xbf]), "+/+/");
        assert_eq!(b64_decode("+/+/", 8), Some(vec![0xfb, 0xff, 0xbf]));
        assert_eq!(b64_decode("-_-_", 8), None);
    }

    #[test]
    fn base64_decode_is_strict() {
        for bad in [
            "Zg",        // no padding
            "Zg=",       // short
            "Zh==",      // non-zero trailing bits
            "Zm9=",      // non-zero trailing bits
            "Zm9v ",     // whitespace
            "Zm9v\n",    // newline
            "=Zg=",      // padding inside a group
            "Z===",      // too much padding
            "Zg==Zm9v",  // padding before the end
            "Zm9\u{e9}", // not ASCII
        ] {
            assert_eq!(b64_decode(bad, 64), None, "{bad:?}");
        }
    }

    #[test]
    fn base64_decode_is_bounded() {
        assert_eq!(b64_decode("Zm9vYmFy", 8), Some(b"foobar".to_vec()));
        assert_eq!(b64_decode("Zm9vYmFy", 7), None);
    }

    #[test]
    fn base64_round_trips_every_length() {
        for len in 0..40u8 {
            let data: Vec<u8> = (0..len)
                .map(|i| i.wrapping_mul(37).wrapping_add(11))
                .collect();
            assert_eq!(b64_decode(&b64_encode(&data), 200), Some(data));
        }
    }

    #[test]
    fn percent_encoding_keeps_only_unreserved_characters() {
        assert_eq!(percent_encode("AZaz09-._~"), "AZaz09-._~");
        assert_eq!(percent_encode("a b&c=d/e?f#g"), "a%20b%26c%3Dd%2Fe%3Ff%23g");
        assert_eq!(percent_encode("https://x.y/z"), "https%3A%2F%2Fx.y%2Fz");
        assert_eq!(percent_encode("é+%"), "%C3%A9%2B%25");
        assert_eq!(percent_encode(""), "");
    }
}
