//! The resumable upload as a state machine with no I/O. The Worker adapter does what
//! each `Step` says (read a range from R2 and PUT it, or ask where the upload stands)
//! and feeds the answer back as a `Reply`; every decision is made here.
//!
//! Safety rule (W8): the video exists at the provider only once the last byte is
//! accepted. So a session that is gone, with the final chunk never sent, is safe to
//! start over (`Transient`); once the final chunk may have reached the provider, or any
//! reply was lost, the outcome is `Unknown` unless a status check settles it.

use crate::publish::is_valid_external_id;
use crate::publish_app::{PublishError, PublishedRef};
use crate::youtube::CHUNK_BYTES;

/// One PUT of the upload: bytes `start..=end` of `total`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk {
    pub start: u64,
    /// Inclusive.
    pub end: u64,
    pub total: u64,
}

impl Chunk {
    pub fn byte_len(&self) -> u64 {
        self.end - self.start + 1
    }

    pub fn is_last(&self) -> bool {
        self.end + 1 == self.total
    }

    /// The `Content-Range` header value, such as `bytes 0-8388607/20971520`.
    pub fn content_range(&self) -> String {
        format!("bytes {}-{}/{}", self.start, self.end, self.total)
    }
}

/// The `Content-Range` of an empty PUT that asks how much the provider has.
pub fn status_content_range(total: u64) -> String {
    format!("bytes */{total}")
}

/// The chunk that starts at `offset`: `CHUNK_BYTES` long, or what is left. Every chunk
/// but the last is a multiple of 256 KiB because `CHUNK_BYTES` is.
pub fn chunk_at(offset: u64, total: u64) -> Option<Chunk> {
    if offset >= total {
        return None;
    }
    let end = offset.saturating_add(CHUNK_BYTES).min(total) - 1;
    Some(Chunk {
        start: offset,
        end,
        total,
    })
}

/// How many bytes the provider says it holds, from a 308's `Range` header. No header
/// means none yet. `bytes=0-N` means `N + 1`. Anything else, or more than `total`, is
/// `None`.
pub fn confirmed_bytes(range: Option<&str>, total: u64) -> Option<u64> {
    let Some(text) = range else { return Some(0) };
    let upper = text.trim().strip_prefix("bytes=0-")?;
    if upper.is_empty() || upper.len() > 15 || !upper.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let received = upper.parse::<u64>().ok()?.checked_add(1)?;
    (received <= total).then_some(received)
}

/// The video resource's `id`, as we are willing to store it. A success reply without a
/// usable id is `Unknown`: the video may exist.
pub fn finish(id: Option<&str>) -> Result<PublishedRef, PublishError> {
    match id {
        Some(id) if is_valid_external_id(id) => Ok(PublishedRef {
            external_id: id.to_string(),
        }),
        _ => Err(PublishError::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn chunks_are_8_mib_and_the_last_takes_what_is_left() {
        let c = chunk_at(0, 20 * MIB).unwrap();
        assert_eq!(
            (c.start, c.end, c.byte_len(), c.is_last()),
            (0, 8 * MIB - 1, 8 * MIB, false)
        );
        assert_eq!(c.content_range(), "bytes 0-8388607/20971520");
        let last = chunk_at(16 * MIB, 20 * MIB).unwrap();
        assert_eq!((last.byte_len(), last.is_last()), (4 * MIB, true));
        assert_eq!(last.content_range(), "bytes 16777216-20971519/20971520");
        let tiny = chunk_at(0, 1).unwrap();
        assert_eq!((tiny.start, tiny.end, tiny.is_last()), (0, 0, true));
        assert_eq!(chunk_at(5, 5), None);
        assert_eq!(chunk_at(0, 0), None);
        assert_eq!(status_content_range(2_000_000), "bytes */2000000");
        // Every chunk but the last is a multiple of 256 KiB, even from an odd offset.
        let from_odd = chunk_at(262_144 + 7, 100 * MIB).unwrap();
        assert_eq!(from_odd.byte_len() % (256 * 1024), 0);
    }

    #[test]
    fn the_range_header_says_how_much_the_provider_holds() {
        assert_eq!(confirmed_bytes(None, 100), Some(0));
        assert_eq!(confirmed_bytes(Some("bytes=0-0"), 100), Some(1));
        assert_eq!(confirmed_bytes(Some("bytes=0-99"), 100), Some(100));
        assert_eq!(confirmed_bytes(Some(" bytes=0-49 "), 100), Some(50));
        for bad in [
            "",
            "bytes=1-5",
            "bytes=0-",
            "bytes=0--1",
            "bytes=0-1,5-9",
            "0-5",
            "bytes=0-100",
            "bytes=0-x",
            "bytes=0-99999999999999999999",
        ] {
            assert_eq!(confirmed_bytes(Some(bad), 100), None, "{bad:?}");
        }
    }

    #[test]
    fn finishing_takes_only_a_well_formed_id() {
        assert_eq!(
            finish(Some("dQw4w9WgXcQ")).unwrap().external_id,
            "dQw4w9WgXcQ"
        );
        for bad in [
            None,
            Some(""),
            Some("a b"),
            Some("<x>"),
            Some(&"a".repeat(33)[..]),
        ] {
            assert_eq!(finish(bad), Err(PublishError::Unknown), "{bad:?}");
        }
    }
}
