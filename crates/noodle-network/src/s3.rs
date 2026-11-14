//! A client for an S3-compatible bucket, as far as publishing needs it: HEAD,
//! GET and a conditional PUT, signed with Signature V4. Written against R2 (region
//! `auto`, path-style URLs).

use std::time::Duration;

use crate::sigv4::{self, Signing};

/// What a [`Bucket::put`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Put {
    Stored,
    /// `If-None-Match: *` found the key taken, so nothing was written.
    Exists,
}

/// A Cloudflare R2 bucket.
pub struct Bucket {
    host: String,
    bucket: String,
    access_key: String,
    secret_key: String,
    agent: ureq::Agent,
}

impl Bucket {
    pub fn new(
        account_id: &str,
        bucket: &str,
        access_key: impl Into<String>,
        secret_key: impl Into<String>,
    ) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            // 404 and 412 are answers here, not failures.
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(600)))
            .build()
            .into();
        Self {
            host: format!("{account_id}.r2.cloudflarestorage.com"),
            bucket: bucket.to_owned(),
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            agent,
        }
    }

    /// Reads `R2_ACCOUNT_ID`, `R2_BUCKET`, `R2_ACCESS_KEY_ID` and
    /// `R2_SECRET_ACCESS_KEY`.
    pub fn from_env() -> Result<Self, String> {
        let var = |name: &str| {
            std::env::var(name)
                .ok()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("{name} is not set"))
        };
        Ok(Self::new(
            &var("R2_ACCOUNT_ID")?,
            &var("R2_BUCKET")?,
            var("R2_ACCESS_KEY_ID")?,
            var("R2_SECRET_ACCESS_KEY")?,
        ))
    }

    fn path(&self, key: &str) -> String {
        let key: Vec<_> = key.split('/').map(sigv4::encode_segment).collect();
        format!("/{}/{}", sigv4::encode_segment(&self.bucket), key.join("/"))
    }

    /// The headers of a request, signed. `extra` are sent but not signed.
    fn sign(&self, method: &str, path: &str, payload: &[u8]) -> (String, String, String) {
        let amz_date = amz_date_now();
        let payload_sha256 = sigv4::sha256_hex(payload);
        let auth = sigv4::authorization(&Signing {
            method,
            path,
            headers: &[
                ("host", &self.host),
                ("x-amz-content-sha256", &payload_sha256),
                ("x-amz-date", &amz_date),
            ],
            payload_sha256: &payload_sha256,
            amz_date: &amz_date,
            region: "auto",
            access_key: &self.access_key,
            secret_key: &self.secret_key,
        });
        (auth, amz_date, payload_sha256)
    }
}

impl Bucket {
    /// The size of the object at `key`, or `None` if there is none.
    pub fn size(&self, key: &str) -> Result<Option<u64>, String> {
        let path = self.path(key);
        let (auth, date, hash) = self.sign("HEAD", &path, b"");
        let response = self
            .agent
            .head(format!("https://{}{path}", self.host))
            .header("x-amz-date", date)
            .header("x-amz-content-sha256", hash)
            .header("Authorization", auth)
            .call()
            .map_err(|e| e.to_string())?;
        match response.status().as_u16() {
            200 => Ok(response.body().content_length()),
            404 => Ok(None),
            other => Err(format!("HEAD {key}: status {other}")),
        }
    }

    /// The object at `key`, or `None` if there is none.
    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        let path = self.path(key);
        let (auth, date, hash) = self.sign("GET", &path, b"");
        let mut response = self
            .agent
            .get(format!("https://{}{path}", self.host))
            .header("x-amz-date", date)
            .header("x-amz-content-sha256", hash)
            .header("Authorization", auth)
            .call()
            .map_err(|e| e.to_string())?;
        match response.status().as_u16() {
            200 => response
                .body_mut()
                .with_config()
                .limit(2 << 30)
                .read_to_vec()
                .map(Some)
                .map_err(|e| e.to_string()),
            404 => Ok(None),
            other => Err(format!("GET {key}: status {other}")),
        }
    }

    /// Writes `body` at `key`. With `only_new` the write is conditional
    /// (`If-None-Match: *`) and a key that exists is left alone.
    pub fn put(
        &self,
        key: &str,
        body: &[u8],
        content_type: &str,
        cache_control: &str,
        only_new: bool,
    ) -> Result<Put, String> {
        let path = self.path(key);
        let (auth, date, hash) = self.sign("PUT", &path, body);
        let mut request = self
            .agent
            .put(format!("https://{}{path}", self.host))
            .header("x-amz-date", date)
            .header("x-amz-content-sha256", hash)
            .header("Authorization", auth)
            .header("Content-Type", content_type)
            .header("Cache-Control", cache_control);
        if only_new {
            request = request.header("If-None-Match", "*");
        }
        let mut response = request.send(body).map_err(|e| e.to_string())?;
        match response.status().as_u16() {
            200..=299 => Ok(Put::Stored),
            // The key is taken, which is what the condition is for.
            409 | 412 if only_new => Ok(Put::Exists),
            other => {
                let text = response.body_mut().read_to_string().unwrap_or_default();
                Err(format!("PUT {key}: status {other}: {text}"))
            }
        }
    }
}

/// The current time as `YYYYMMDDTHHMMSSZ`.
fn amz_date_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after 1970")
        .as_secs() as i64;
    amz_date(secs)
}

fn amz_date(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::amz_date;

    #[test]
    fn formats_dates() {
        assert_eq!(amz_date(0), "19700101T000000Z");
        assert_eq!(amz_date(1_369_353_600), "20130524T000000Z");
        // A leap day.
        assert_eq!(amz_date(1_709_210_096), "20240229T123456Z");
    }
}
