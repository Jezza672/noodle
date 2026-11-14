//! AWS Signature Version 4 for S3-compatible stores (R2 takes it with region
//! `auto`). Only what uploading needs: no query string, headers signed as given.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(data.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// What to sign: a request with no query string.
pub struct Signing<'a> {
    pub method: &'a str,
    /// The URI path, already percent-encoded, starting with `/`.
    pub path: &'a str,
    /// Lowercase names and trimmed values, including `host`,
    /// `x-amz-content-sha256` and `x-amz-date`.
    pub headers: &'a [(&'a str, &'a str)],
    pub payload_sha256: &'a str,
    /// `YYYYMMDDTHHMMSSZ`.
    pub amz_date: &'a str,
    pub region: &'a str,
    pub access_key: &'a str,
    pub secret_key: &'a str,
}

/// The value of the `Authorization` header.
pub fn authorization(s: &Signing) -> String {
    let mut headers: Vec<_> = s.headers.to_vec();
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed = headers
        .iter()
        .map(|(k, _)| *k)
        .collect::<Vec<_>>()
        .join(";");
    let canonical = format!(
        "{}\n{}\n\n{canonical_headers}\n{signed}\n{}",
        s.method, s.path, s.payload_sha256
    );
    let date = &s.amz_date[..8];
    let scope = format!("{date}/{}/s3/aws4_request", s.region);
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{scope}\n{}",
        s.amz_date,
        sha256_hex(canonical.as_bytes())
    );
    let k = hmac(format!("AWS4{}", s.secret_key).as_bytes(), date);
    let k = hmac(&k, s.region);
    let k = hmac(&k, "s3");
    let k = hmac(&k, "aws4_request");
    let signature = hex(&hmac(&k, &to_sign));
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed}, Signature={signature}",
        s.access_key
    )
}

/// Percent-encodes one path segment as S3 wants: everything but unreserved
/// characters.
pub fn encode_segment(segment: &str) -> String {
    let mut out = String::new();
    for b in segment.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The "GET Object" example in AWS's Signature Version 4 documentation for S3.
    #[test]
    fn matches_the_documented_example() {
        let empty = sha256_hex(b"");
        let auth = authorization(&Signing {
            method: "GET",
            path: "/test.txt",
            headers: &[
                ("host", "examplebucket.s3.amazonaws.com"),
                ("range", "bytes=0-9"),
                ("x-amz-content-sha256", &empty),
                ("x-amz-date", "20130524T000000Z"),
            ],
            payload_sha256: &empty,
            amz_date: "20130524T000000Z",
            region: "us-east-1",
            access_key: "AKIAIOSFODNN7EXAMPLE",
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        });
        assert!(
            auth.ends_with(
                "Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
            ),
            "{auth}"
        );
        assert!(auth.contains(
            "Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, SignedHeaders=host;range;x-amz-content-sha256;x-amz-date"
        ));
    }

    #[test]
    fn encodes_what_s3_wants_encoded() {
        assert_eq!(encode_segment("a b+c/é"), "a%20b%2Bc%2F%C3%A9");
        assert_eq!(encode_segment("ok-1_2.3~"), "ok-1_2.3~");
    }
}
