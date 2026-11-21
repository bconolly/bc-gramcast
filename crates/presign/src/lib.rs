//! AWS Signature Version 4 query-string presigning, used for Cloudflare R2.
//!
//! Cloudflare's R2 Workers binding cannot generate presigned URLs, so we sign
//! them ourselves (R2 docs, "Presigned URLs", updated 12/11/2026).

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// R2 allows presigned URLs from 1 second to 7 days.
pub const MAX_EXPIRES_SECS: u32 = 604_800;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Put,
}

impl Method {
    fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Put => "PUT",
        }
    }
}

#[derive(Clone)]
pub struct Credentials<'a> {
    pub access_key_id: &'a str,
    pub secret_access_key: &'a str,
}

impl std::fmt::Debug for Credentials<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct PresignRequest<'a> {
    pub method: Method,
    /// Host only, for example `<account>.r2.cloudflarestorage.com`.
    pub host: &'a str,
    /// Path starting with `/`, for example `/bucket/key.mp4`.
    pub path: &'a str,
    /// Region. R2 uses `auto`.
    pub region: &'a str,
    /// Timestamp in `YYYYMMDDTHHMMSSZ` form.
    pub amz_date: &'a str,
    pub expires_secs: u32,
    /// When set, `content-type` is a signed header, so an upload must send exactly this type.
    pub content_type: Option<&'a str>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PresignError {
    ExpiryOutOfRange(u32),
    BadDate,
    BadPath,
    BadHost,
    BadRegion,
    BadContentType,
}

/// Returns a full `https://` presigned URL.
pub fn presign(creds: &Credentials, req: &PresignRequest) -> Result<String, PresignError> {
    if req.expires_secs == 0 || req.expires_secs > MAX_EXPIRES_SECS {
        return Err(PresignError::ExpiryOutOfRange(req.expires_secs));
    }
    if !is_amz_date(req.amz_date) {
        return Err(PresignError::BadDate);
    }
    if !req.path.starts_with('/')
        || req.path.split('/').any(|seg| seg == ".." || seg == ".")
        || req
            .path
            .bytes()
            .any(|b| b.is_ascii_control() || b == b'?' || b == b'#')
    {
        return Err(PresignError::BadPath);
    }
    if !is_host(req.host) {
        return Err(PresignError::BadHost);
    }
    if req.region.is_empty()
        || !req
            .region
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(PresignError::BadRegion);
    }
    if let Some(ct) = req.content_type {
        if ct.is_empty() || ct.bytes().any(|b| b.is_ascii_control()) {
            return Err(PresignError::BadContentType);
        }
    }
    let (signed_headers, canonical_headers) = match req.content_type {
        Some(ct) => (
            "content-type;host",
            format!("content-type:{}\nhost:{}\n", ct.trim(), req.host),
        ),
        None => ("host", format!("host:{}\n", req.host)),
    };
    let date = &req.amz_date[..8];
    let scope = format!("{date}/{}/s3/aws4_request", req.region);
    let credential = format!("{}/{scope}", creds.access_key_id);

    // Query parameters, already in sorted order.
    let query = format!(
        "X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential={}&X-Amz-Date={}&X-Amz-Expires={}&X-Amz-SignedHeaders={}",
        uri_encode(&credential, true),
        req.amz_date,
        req.expires_secs,
        uri_encode(signed_headers, true)
    );
    let canonical_path = uri_encode(req.path, false);
    let canonical_request = format!(
        "{}\n{canonical_path}\n{query}\n{canonical_headers}\n{signed_headers}\nUNSIGNED-PAYLOAD",
        req.method.as_str()
    );
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{scope}\n{}",
        req.amz_date,
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );
    let key = signing_key(creds.secret_access_key, date, req.region);
    let signature = hex::encode(hmac(&key, string_to_sign.as_bytes()));
    Ok(format!(
        "https://{}{canonical_path}?{query}&X-Amz-Signature={signature}",
        req.host
    ))
}

/// Exactly `YYYYMMDDTHHMMSSZ`, ASCII digits only.
fn is_amz_date(d: &str) -> bool {
    let b = d.as_bytes();
    b.len() == 16
        && b[8] == b'T'
        && b[15] == b'Z'
        && b[..8].iter().chain(&b[9..15]).all(u8::is_ascii_digit)
}

/// A DNS host name: letters, digits, dots and hyphens only.
fn is_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && h.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

fn signing_key(secret: &str, date: &str, region: &str) -> Vec<u8> {
    let k_date = hmac(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k_region = hmac(&k_date, region.as_bytes());
    let k_service = hmac(&k_region, b"s3");
    hmac(&k_service, b"aws4_request")
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// SigV4 URI encoding. Unreserved characters pass through. `/` is kept in
/// paths and encoded in query values.
fn uri_encode(input: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        let keep = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~');
        if keep || (b == b'/' && !encode_slash) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Formats Unix seconds (UTC) as `YYYYMMDDTHHMMSSZ`.
pub fn amz_date_from_unix(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_unix_time_as_amz_date() {
        assert_eq!(amz_date_from_unix(1_369_353_600), "20130524T000000Z");
        assert_eq!(amz_date_from_unix(0), "19700101T000000Z");
        // 29/02/2028 13:05:09 UTC, a leap day.
        assert_eq!(amz_date_from_unix(1835442309), "20280229T130509Z");
    }

    const CREDS: Credentials = Credentials {
        access_key_id: "AKIAIOSFODNN7EXAMPLE",
        secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
    };

    fn aws_doc_request() -> PresignRequest<'static> {
        PresignRequest {
            method: Method::Get,
            host: "examplebucket.s3.amazonaws.com",
            path: "/test.txt",
            region: "us-east-1",
            amz_date: "20130524T000000Z",
            expires_secs: 86_400,
            content_type: None,
        }
    }

    #[test]
    fn matches_aws_documented_presigned_signature() {
        // Example from AWS "Authenticating Requests: Using Query Parameters".
        let url = presign(&CREDS, &aws_doc_request()).unwrap();
        assert!(
            url.ends_with(
                "X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404"
            ),
            "{url}"
        );
    }

    #[test]
    fn rejects_expiry_over_seven_days() {
        let mut req = aws_doc_request();
        req.expires_secs = MAX_EXPIRES_SECS + 1;
        assert_eq!(
            presign(&CREDS, &req),
            Err(PresignError::ExpiryOutOfRange(604_801))
        );
    }

    #[test]
    fn rejects_zero_expiry() {
        let mut req = aws_doc_request();
        req.expires_secs = 0;
        assert_eq!(
            presign(&CREDS, &req),
            Err(PresignError::ExpiryOutOfRange(0))
        );
    }

    #[test]
    fn rejects_malformed_date() {
        let mut req = aws_doc_request();
        req.amz_date = "2013-05-24";
        assert_eq!(presign(&CREDS, &req), Err(PresignError::BadDate));
    }

    #[test]
    fn encodes_spaces_in_object_keys() {
        let mut req = aws_doc_request();
        req.path = "/bucket/my reel.mp4";
        let url = presign(&CREDS, &req).unwrap();
        assert!(url.contains("/bucket/my%20reel.mp4?"), "{url}");
    }

    #[test]
    fn debug_output_redacts_the_secret() {
        let shown = format!("{CREDS:?}");
        assert!(!shown.contains("wJalrXUtnFEMI"), "{shown}");
        assert!(shown.contains("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn rejects_dates_that_are_not_strict_digits() {
        for bad in [
            "2013052&T00000Z",
            "20130524T00000\u{e9}Z",
            "2013052400000000Z",
        ] {
            let mut req = aws_doc_request();
            req.amz_date = bad;
            assert_eq!(presign(&CREDS, &req), Err(PresignError::BadDate), "{bad}");
        }
    }

    #[test]
    fn rejects_hosts_and_regions_with_url_characters() {
        let mut req = aws_doc_request();
        req.host = "evil.com/x?";
        assert_eq!(presign(&CREDS, &req), Err(PresignError::BadHost));
        let mut req = aws_doc_request();
        req.region = "auto&x=1";
        assert_eq!(presign(&CREDS, &req), Err(PresignError::BadRegion));
    }

    #[test]
    fn rejects_dot_dot_path_segments() {
        let mut req = aws_doc_request();
        req.path = "/bucket/../other/key.mp4";
        assert_eq!(presign(&CREDS, &req), Err(PresignError::BadPath));
    }

    #[test]
    fn signs_content_type_when_given() {
        let mut req = aws_doc_request();
        req.method = Method::Put;
        let plain = presign(&CREDS, &req).unwrap();
        req.content_type = Some("video/mp4");
        let typed = presign(&CREDS, &req).unwrap();
        assert!(
            typed.contains("X-Amz-SignedHeaders=content-type%3Bhost"),
            "{typed}"
        );
        assert_ne!(plain, typed);
    }

    #[test]
    fn rejects_content_type_with_line_breaks() {
        let mut req = aws_doc_request();
        req.content_type = Some("video/mp4\nx-evil: 1");
        assert_eq!(presign(&CREDS, &req), Err(PresignError::BadContentType));
    }

    #[test]
    fn put_and_get_signatures_differ() {
        let get = presign(&CREDS, &aws_doc_request()).unwrap();
        let mut req = aws_doc_request();
        req.method = Method::Put;
        let put = presign(&CREDS, &req).unwrap();
        assert_ne!(get, put);
    }
}
