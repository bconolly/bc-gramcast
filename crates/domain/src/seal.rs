//! Encrypting the owner's token for storage: the strict payload format and the
//! `TOKEN_ENC_KEY` check.
//!
//! The payload holds the whole token, expiry included, and is the authoritative
//! copy; the `expires_at` column beside it is only for looking at the table.
//! Nothing here logs, and the errors carry no token or key text.

use crate::app::PortError;
use crate::codec;
use crate::connect::MAX_TOKEN_LEN;
use crate::token::StoredToken;

/// The provider name. It is the additional authenticated data, so a row copied
/// under another provider's name fails to decrypt.
pub const PROVIDER_YOUTUBE: &str = "youtube";
/// AES-GCM nonce size.
pub const NONCE_LEN: usize = 12;
/// AES-256 key size.
pub const KEY_LEN: usize = 32;
const PAYLOAD_VERSION: u8 = 1;

#[derive(Debug, PartialEq, Eq)]
pub enum OpenError {
    /// The row is not in the format we write.
    Corrupt,
    /// The cipher refused: wrong key, wrong provider, or an edited row.
    Cipher(PortError),
}

/// The key secret was not 32 bytes of standard base64. Carries no text on purpose.
#[derive(Debug, PartialEq, Eq)]
pub struct KeyError;

/// `TOKEN_ENC_KEY`: standard base64 of exactly 32 bytes (`openssl rand -base64 32`).
/// Surrounding whitespace, as a pasted secret may carry, is ignored.
pub fn parse_enc_key(text: &str) -> Result<[u8; KEY_LEN], KeyError> {
    let bytes = codec::b64_decode(text.trim(), 44).ok_or(KeyError)?;
    <[u8; KEY_LEN]>::try_from(bytes.as_slice()).map_err(|_| KeyError)
}

fn put_text(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&(text.len() as u32).to_be_bytes());
    out.extend_from_slice(text.as_bytes());
}

/// Version byte, `expires_at` (8 bytes big-endian), then the access and refresh
/// tokens, each a 4-byte big-endian length and its UTF-8 bytes.
pub fn encode_payload(token: &StoredToken) -> Vec<u8> {
    let mut out = vec![PAYLOAD_VERSION];
    out.extend_from_slice(&token.expires_at.to_be_bytes());
    put_text(&mut out, &token.access_token);
    put_text(&mut out, &token.refresh_token);
    out
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], OpenError> {
        if self.0.len() < n {
            return Err(OpenError::Corrupt);
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn text(&mut self) -> Result<String, OpenError> {
        let len = u32::from_be_bytes(self.take(4)?.try_into().map_err(|_| OpenError::Corrupt)?);
        let len = usize::try_from(len)
            .ok()
            .filter(|n| *n <= MAX_TOKEN_LEN)
            .ok_or(OpenError::Corrupt)?;
        String::from_utf8(self.take(len)?.to_vec()).map_err(|_| OpenError::Corrupt)
    }
}

/// Strict inverse of `encode_payload`: wrong version, short, long or non-UTF-8 is `Corrupt`.
pub fn decode_payload(bytes: &[u8]) -> Result<StoredToken, OpenError> {
    let mut r = Reader(bytes);
    if r.take(1)? != [PAYLOAD_VERSION] {
        return Err(OpenError::Corrupt);
    }
    let expires_at = i64::from_be_bytes(r.take(8)?.try_into().map_err(|_| OpenError::Corrupt)?);
    let access_token = r.text()?;
    let refresh_token = r.text()?;
    if !r.0.is_empty() {
        return Err(OpenError::Corrupt);
    }
    Ok(StoredToken {
        access_token,
        refresh_token,
        expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token() -> StoredToken {
        StoredToken {
            access_token: "ya29.SECRET-ACCESS".into(),
            refresh_token: "1//SECRET-REFRESH".into(),
            expires_at: 1_700_000_000,
        }
    }

    #[test]
    fn the_payload_round_trips() {
        for t in [
            token(),
            StoredToken {
                refresh_token: String::new(),
                ..token()
            },
            StoredToken {
                access_token: "é€".into(),
                expires_at: -1,
                ..token()
            },
        ] {
            assert_eq!(decode_payload(&encode_payload(&t)), Ok(t));
        }
    }

    #[test]
    fn the_payload_is_versioned_and_length_prefixed() {
        let t = StoredToken {
            access_token: "ab".into(),
            refresh_token: "c".into(),
            expires_at: 258,
        };
        assert_eq!(
            encode_payload(&t),
            [1, 0, 0, 0, 0, 0, 0, 1, 2, 0, 0, 0, 2, b'a', b'b', 0, 0, 0, 1, b'c']
        );
    }

    #[test]
    fn a_bad_payload_is_corrupt() {
        let good = encode_payload(&token());
        let mut cases: Vec<Vec<u8>> = vec![vec![], vec![1], vec![2; 30]];
        for cut in [1, 8, 12, good.len() - 1] {
            cases.push(good[..cut].to_vec());
        }
        let mut trailing = good.clone();
        trailing.push(0);
        cases.push(trailing);
        let mut other_version = good.clone();
        other_version[0] = 2;
        cases.push(other_version);
        let mut bad_utf8 = good.clone();
        bad_utf8[13] = 0xff;
        cases.push(bad_utf8);
        let mut huge = good.clone();
        huge[9..13].copy_from_slice(&u32::MAX.to_be_bytes());
        cases.push(huge);
        for c in cases {
            assert_eq!(decode_payload(&c), Err(OpenError::Corrupt), "{c:?}");
        }
    }

    #[test]
    fn the_key_is_32_bytes_of_standard_base64() {
        let key = [7u8; KEY_LEN];
        let text = codec::b64_encode(&key);
        assert_eq!(text.len(), 44);
        assert_eq!(parse_enc_key(&text), Ok(key));
        assert_eq!(parse_enc_key(&format!("  {text}\n")), Ok(key));
    }

    #[test]
    fn a_bad_key_is_refused() {
        for bad in [
            String::new(),
            "short".into(),
            codec::b64_encode(&[1u8; 31]),
            codec::b64_encode(&[1u8; 33]),
            codec::b64_encode(&[1u8; 16]),
            "-".repeat(44),
            codec::b64_encode(&[0xfbu8; 32]).replace('+', "-"),
        ] {
            assert_eq!(parse_enc_key(&bad), Err(KeyError), "{bad:?}");
        }
    }
}
