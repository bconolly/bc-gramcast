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

#[derive(Debug, Clone)]
pub struct Credentials<'a> {
    pub access_key_id: &'a str,
    pub secret_access_key: &'a str,
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
}

#[derive(Debug, PartialEq, Eq)]
pub enum PresignError {
    ExpiryOutOfRange(u32),
    BadDate,
    BadPath,
}

/// Returns a full `https://` presigned URL.
pub fn presign(creds: &Credentials, req: &PresignRequest) -> Result<String, PresignError> {
    if req.expires_secs == 0 || req.expires_secs > MAX_EXPIRES_SECS {
        return Err(PresignError::ExpiryOutOfRange(req.expires_secs));
    }
    if req.amz_date.len() != 16 || !req.amz_date.ends_with('Z') {
        return Err(PresignError::BadDate);
    }
    if !req.path.starts_with('/') {
        return Err(PresignError::BadPath);
    }
    let date = &req.amz_date[..8];
    let scope = format!("{date}/{}/s3/aws4_request", req.region);
    let credential = format!("{}/{scope}", creds.access_key_id);

    // Query parameters, already in sorted order.
    let query = format!(
        "X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential={}&X-Amz-Date={}&X-Amz-Expires={}&X-Amz-SignedHeaders=host",
        uri_encode(&credential, true),
        req.amz_date,
        req.expires_secs
    );
    let canonical_path = uri_encode(req.path, false);
    let canonical_request = format!(
        "{}\n{canonical_path}\n{query}\nhost:{}\n\nhost\nUNSIGNED-PAYLOAD",
        req.method.as_str(),
        req.host
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn put_and_get_signatures_differ() {
        let get = presign(&CREDS, &aws_doc_request()).unwrap();
        let mut req = aws_doc_request();
        req.method = Method::Put;
        let put = presign(&CREDS, &req).unwrap();
        assert_ne!(get, put);
    }
}
