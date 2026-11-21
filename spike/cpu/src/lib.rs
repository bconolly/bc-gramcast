//! Spike: how long does one presign take inside V8 WebAssembly?
use presign::{presign, Credentials, Method, PresignRequest};

#[no_mangle]
pub extern "C" fn sign_n(n: u32) -> u32 {
    let creds = Credentials { access_key_id: "AKIAIOSFODNN7EXAMPLE", secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY" };
    let mut total = 0u32;
    for _ in 0..n {
        let req = PresignRequest { method: Method::Get, host: "acct.r2.cloudflarestorage.com", path: "/videos/reel-1.mp4", region: "auto", amz_date: "20261122T000000Z", expires_secs: 3600 };
        total = total.wrapping_add(presign(&creds, &req).map(|u| u.len() as u32).unwrap_or(0));
    }
    total
}
