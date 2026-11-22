//! Encrypting the owner's token for storage. The cipher (AES-256-GCM through Web
//! Crypto in the Worker) is a port; the row format, the nonce rule and the
//! additional authenticated data are decided here.
//!
//! The payload holds the whole token, expiry included, and is the authoritative
//! copy; the `expires_at` column beside it is only for looking at the table.
//! Nothing here logs, and the errors carry no token or key text.

use crate::app::PortError;
use crate::codec;
use crate::connect::MAX_TOKEN_LEN;
use crate::oauth::RandomSource;
use crate::token::StoredToken;

/// The provider name. It is the additional authenticated data, so a row copied
/// under another provider's name fails to decrypt.
pub const PROVIDER_YOUTUBE: &str = "youtube";
/// AES-GCM nonce size.
pub const NONCE_LEN: usize = 12;
/// AES-256 key size.
pub const KEY_LEN: usize = 32;
const PAYLOAD_VERSION: u8 = 1;
/// Base64 of 12 bytes.
const NONCE_B64_LEN: usize = 16;
/// Longest ciphertext text read back: two tokens, the framing and the GCM tag, in base64.
const MAX_CIPHERTEXT_B64: usize = (2 * MAX_TOKEN_LEN + 64) / 3 * 4 + 8;

/// Port: AES-256-GCM with the key held by the adapter. `cipher` carries the
/// 16-byte tag; a wrong key, nonce or AAD, or an edited byte, is an error.
#[allow(async_fn_in_trait)]
pub trait TokenCipher {
    async fn encrypt(
        &self,
        nonce: &[u8; NONCE_LEN],
        aad: &[u8],
        plain: &[u8],
    ) -> Result<Vec<u8>, PortError>;
    async fn decrypt(
        &self,
        nonce: &[u8; NONCE_LEN],
        aad: &[u8],
        cipher: &[u8],
    ) -> Result<Vec<u8>, PortError>;
}

/// The two text columns an encrypted token row holds (standard base64).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedToken {
    pub ciphertext: String,
    pub nonce: String,
}

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

/// Encrypts a token under a fresh random nonce. Every write draws its own nonce,
/// so the same token never produces the same row twice.
pub async fn seal_token<C: TokenCipher, R: RandomSource>(
    cipher: &C,
    rng: &R,
    provider: &str,
    token: &StoredToken,
) -> Result<SealedToken, PortError> {
    if token.access_token.len() > MAX_TOKEN_LEN || token.refresh_token.len() > MAX_TOKEN_LEN {
        return Err(PortError("token too large to store".into()));
    }
    let mut nonce = [0u8; NONCE_LEN];
    rng.fill(&mut nonce)?;
    let sealed = cipher
        .encrypt(&nonce, provider.as_bytes(), &encode_payload(token))
        .await?;
    Ok(SealedToken {
        ciphertext: codec::b64_encode(&sealed),
        nonce: codec::b64_encode(&nonce),
    })
}

/// Reads a stored row back. Malformed fields are `Corrupt` before the cipher is touched.
pub async fn open_token<C: TokenCipher>(
    cipher: &C,
    provider: &str,
    row: &SealedToken,
) -> Result<StoredToken, OpenError> {
    let nonce = codec::b64_decode(&row.nonce, NONCE_B64_LEN)
        .and_then(|n| <[u8; NONCE_LEN]>::try_from(n).ok())
        .ok_or(OpenError::Corrupt)?;
    let sealed = codec::b64_decode(&row.ciphertext, MAX_CIPHERTEXT_B64)
        .filter(|c| !c.is_empty())
        .ok_or(OpenError::Corrupt)?;
    let plain = cipher
        .decrypt(&nonce, provider.as_bytes(), &sealed)
        .await
        .map_err(OpenError::Cipher)?;
    decode_payload(&plain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::RandomSource;
    use std::cell::{Cell, RefCell};

    fn token() -> StoredToken {
        StoredToken {
            access_token: "ya29.SECRET-ACCESS".into(),
            refresh_token: "1//SECRET-REFRESH".into(),
            expires_at: 1_700_000_000,
        }
    }

    /// Not real crypto: hides the bytes with the nonce and appends a tag over
    /// (nonce, aad, plaintext), so a wrong nonce, AAD or edit fails to decrypt.
    #[derive(Default)]
    struct FakeCipher {
        seen: RefCell<Vec<(Vec<u8>, Vec<u8>)>>,
        broken: bool,
    }
    fn tag(nonce: &[u8; NONCE_LEN], aad: &[u8], plain: &[u8]) -> [u8; 8] {
        let hash = nonce
            .iter()
            .chain(aad)
            .chain(plain)
            .fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
                (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
            });
        hash.to_be_bytes()
    }
    impl TokenCipher for FakeCipher {
        async fn encrypt(
            &self,
            nonce: &[u8; NONCE_LEN],
            aad: &[u8],
            plain: &[u8],
        ) -> Result<Vec<u8>, PortError> {
            if self.broken {
                return Err(PortError("subtle down".into()));
            }
            self.seen.borrow_mut().push((nonce.to_vec(), aad.to_vec()));
            let mut out: Vec<u8> = plain.iter().map(|b| b ^ nonce[0] ^ 0x5a).collect();
            out.extend_from_slice(&tag(nonce, aad, plain));
            Ok(out)
        }
        async fn decrypt(
            &self,
            nonce: &[u8; NONCE_LEN],
            aad: &[u8],
            cipher: &[u8],
        ) -> Result<Vec<u8>, PortError> {
            let Some(split) = cipher.len().checked_sub(8) else {
                return Err(PortError("short".into()));
            };
            let (body, last) = cipher.split_at(split);
            let plain: Vec<u8> = body.iter().map(|b| b ^ nonce[0] ^ 0x5a).collect();
            if last != tag(nonce, aad, &plain) {
                return Err(PortError("auth failed".into()));
            }
            Ok(plain)
        }
    }

    #[derive(Default)]
    struct FakeRandom {
        calls: Cell<u8>,
        fail: bool,
    }
    impl RandomSource for FakeRandom {
        fn fill(&self, buf: &mut [u8]) -> Result<(), PortError> {
            if self.fail {
                return Err(PortError("rng down".into()));
            }
            self.calls.set(self.calls.get() + 1);
            buf.fill(self.calls.get());
            Ok(())
        }
    }

    fn seal(c: &FakeCipher, r: &FakeRandom, t: &StoredToken) -> Result<SealedToken, PortError> {
        pollster::block_on(seal_token(c, r, PROVIDER_YOUTUBE, t))
    }
    fn open(c: &FakeCipher, provider: &str, s: &SealedToken) -> Result<StoredToken, OpenError> {
        pollster::block_on(open_token(c, provider, s))
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
    fn a_sealed_token_opens_again() {
        let (c, r) = (FakeCipher::default(), FakeRandom::default());
        let sealed = seal(&c, &r, &token()).unwrap();
        assert_eq!(open(&c, PROVIDER_YOUTUBE, &sealed), Ok(token()));
    }

    #[test]
    fn the_row_fields_are_base64_and_hide_the_tokens() {
        let (c, r) = (FakeCipher::default(), FakeRandom::default());
        let sealed = seal(&c, &r, &token()).unwrap();
        assert_eq!(sealed.nonce.len(), 16);
        assert_eq!(
            codec::b64_decode(&sealed.nonce, 16).unwrap().len(),
            NONCE_LEN
        );
        assert!(codec::b64_decode(&sealed.ciphertext, 100_000).is_some());
        assert!(!sealed.ciphertext.contains("SECRET"));
    }

    #[test]
    fn the_provider_name_is_the_additional_authenticated_data() {
        let (c, r) = (FakeCipher::default(), FakeRandom::default());
        let sealed = seal(&c, &r, &token()).unwrap();
        assert_eq!(c.seen.borrow()[0].1, b"youtube");
        assert_eq!(PROVIDER_YOUTUBE, "youtube");
        // A row copied under another provider's name does not open.
        assert!(matches!(
            open(&c, "instagram", &sealed),
            Err(OpenError::Cipher(_))
        ));
    }

    #[test]
    fn every_write_gets_a_fresh_random_nonce() {
        let (c, r) = (FakeCipher::default(), FakeRandom::default());
        let a = seal(&c, &r, &token()).unwrap();
        let b = seal(&c, &r, &token()).unwrap();
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.ciphertext, b.ciphertext);
        let seen = c.seen.borrow();
        assert_eq!(seen[0].0.len(), NONCE_LEN);
        assert_ne!(seen[0].0, seen[1].0);
    }

    #[test]
    fn a_tampered_row_does_not_open() {
        let (c, r) = (FakeCipher::default(), FakeRandom::default());
        let sealed = seal(&c, &r, &token()).unwrap();
        let mut bytes = codec::b64_decode(&sealed.ciphertext, 100_000).unwrap();
        bytes[0] ^= 1;
        let edited = SealedToken {
            ciphertext: codec::b64_encode(&bytes),
            ..sealed.clone()
        };
        assert!(matches!(
            open(&c, PROVIDER_YOUTUBE, &edited),
            Err(OpenError::Cipher(_))
        ));
        let other_nonce = SealedToken {
            nonce: codec::b64_encode(&[9u8; NONCE_LEN]),
            ..sealed
        };
        assert!(matches!(
            open(&c, PROVIDER_YOUTUBE, &other_nonce),
            Err(OpenError::Cipher(_))
        ));
    }

    #[test]
    fn malformed_row_fields_are_corrupt_and_never_reach_the_cipher() {
        let (c, r) = (FakeCipher::default(), FakeRandom::default());
        let good = seal(&c, &r, &token()).unwrap();
        let bad_rows = [
            SealedToken {
                nonce: "AAAA".into(),
                ..good.clone()
            },
            SealedToken {
                nonce: "not base64!!!!!!!".into(),
                ..good.clone()
            },
            SealedToken {
                ciphertext: "***".into(),
                ..good.clone()
            },
            SealedToken {
                ciphertext: String::new(),
                ..good.clone()
            },
        ];
        let before = c.seen.borrow().len();
        for row in bad_rows {
            assert_eq!(open(&c, PROVIDER_YOUTUBE, &row), Err(OpenError::Corrupt));
        }
        assert_eq!(c.seen.borrow().len(), before);
    }

    #[test]
    fn cipher_and_random_failures_are_errors() {
        let broken = FakeCipher {
            broken: true,
            ..Default::default()
        };
        assert_eq!(
            seal(&broken, &FakeRandom::default(), &token()),
            Err(PortError("subtle down".into()))
        );
        let rng = FakeRandom {
            fail: true,
            ..Default::default()
        };
        assert_eq!(
            seal(&FakeCipher::default(), &rng, &token()),
            Err(PortError("rng down".into()))
        );
    }

    #[test]
    fn an_oversized_token_is_not_sealed() {
        let big = StoredToken {
            access_token: "a".repeat(MAX_TOKEN_LEN + 1),
            ..token()
        };
        assert!(seal(&FakeCipher::default(), &FakeRandom::default(), &big).is_err());
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
