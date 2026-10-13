use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::model::{looks_like_image, parse_chapter, parse_manga, sort_chapters, Chapter, Manga};

pub const API: &str = "https://api.mangadex.org";
const REPORT_URL: &str = "https://api.mangadex.network/report";
const USER_AGENT: &str = concat!(
    "mangadex-dl/",
    env!("CARGO_PKG_VERSION"),
    " (personal non-commercial client; +https://github.com/AbobaAI/mangadex-dl)"
);
const RATINGS: &[&str] = &["safe", "suggestive", "erotica", "pornographic"];

#[derive(Debug)]
pub struct Error {
    pub message: String,
    pub status: Option<u16>,
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for Error {}

impl Error {
    fn new(message: impl Into<String>, status: Option<u16>) -> Self {
        Self {
            message: message.into(),
            status,
        }
    }
}

#[derive(Debug)]
pub enum Fail {
    Api(Error),
    Cancelled,
    Pipe,
    Message(String),
}

impl std::fmt::Display for Fail {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Api(error) => write!(formatter, "{error}"),
            Self::Cancelled => write!(formatter, "Остановлено"),
            Self::Pipe => write!(formatter, "канал закрыт"),
            Self::Message(message) => write!(formatter, "{message}"),
        }
    }
}

impl From<Error> for Fail {
    fn from(error: Error) -> Self {
        Self::Api(error)
    }
}

impl From<String> for Fail {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

struct Limiter {
    interval: Duration,
    next: Mutex<Instant>,
}

impl Limiter {
    fn new(interval: Duration) -> Self {
        Self {
            interval,
            next: Mutex::new(Instant::now()),
        }
    }

    fn wait(&self) {
        let mut next = self.next.lock().expect("limiter");
        let now = Instant::now();
        if now < *next {
            thread::sleep(*next - now);
        }
        *next = Instant::now() + self.interval;
    }
}

pub struct Client {
    http: reqwest::blocking::Client,
    api_limiter: Limiter,
    image_limiter: Limiter,
}

impl Client {
    pub fn new() -> Result<Self, Error> {
        let http = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .redirect(reqwest::redirect::Policy::limited(8))
            .build()
            .map_err(|error| Error::new(format!("сеть: {error}"), None))?;
        Ok(Self {
            http,
            api_limiter: Limiter::new(Duration::from_millis(250)),
            image_limiter: Limiter::new(Duration::from_millis(200)),
        })
    }

    pub fn get_manga(&self, manga_id: &str, lang: &str) -> Result<Manga, Error> {
        let payload = self.get_json(&format!("{API}/manga/{manga_id}?includes[]=cover_art"))?;
        parse_manga(&payload["data"], lang).map_err(|message| Error::new(message, None))
    }

    pub fn search(&self, title: &str, lang: &str) -> Result<(Vec<Manga>, usize), Error> {
        let mut pairs = vec![
            ("title", title.to_string()),
            ("limit", "15".to_string()),
            ("order[relevance]", "desc".to_string()),
            ("includes[]", "cover_art".to_string()),
        ];
        for rating in RATINGS {
            pairs.push(("contentRating[]", (*rating).to_string()));
        }
        let payload = self.get_json(&format!("{API}/manga?{}", query(&pairs)))?;
        let found = payload
            .get("data")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| parse_manga(item, lang).ok())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let total = payload
            .get("total")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(found.len());
        Ok((found, total))
    }

    pub fn chapters(&self, manga_id: &str, lang: &str) -> Result<Vec<Chapter>, Error> {
        let mut found = Vec::new();
        let mut offset = 0usize;
        let mut total: Option<usize> = None;
        loop {
            if total.is_some_and(|total| offset >= total) {
                break;
            }
            let mut pairs = vec![
                ("limit", "100".to_string()),
                ("offset", offset.to_string()),
                ("translatedLanguage[]", lang.to_string()),
                ("order[volume]", "asc".to_string()),
                ("order[chapter]", "asc".to_string()),
                ("includeFuturePublishAt", "0".to_string()),
                ("includeEmptyPages", "0".to_string()),
                ("includes[]", "scanlation_group".to_string()),
                ("includes[]", "user".to_string()),
            ];
            for rating in RATINGS {
                pairs.push(("contentRating[]", (*rating).to_string()));
            }
            let payload =
                self.get_json(&format!("{API}/manga/{manga_id}/feed?{}", query(&pairs)))?;
            let batch = payload
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            total = Some(payload.get("total").and_then(Value::as_u64).unwrap_or(0) as usize);
            if batch.is_empty() {
                break;
            }
            offset += batch.len();
            for item in batch {
                found.push(parse_chapter(&item).map_err(|message| Error::new(message, None))?);
            }
        }
        Ok(sort_chapters(found))
    }

    pub fn chapter_with_manga(
        &self,
        chapter_id: &str,
        lang: &str,
    ) -> Result<(Manga, Chapter), Error> {
        let pairs = [
            ("includes[]", "scanlation_group"),
            ("includes[]", "user"),
            ("includes[]", "manga"),
        ];
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        let payload =
            self.get_json(&format!("{API}/chapter/{chapter_id}?{}", query_str(&owned)))?;
        let obj = payload.get("data").cloned().unwrap_or(Value::Null);
        let chapter = parse_chapter(&obj).map_err(|message| Error::new(message, None))?;
        let mut manga_obj = None;
        let mut manga_id = None;
        if let Some(relations) = obj.get("relationships").and_then(Value::as_array) {
            for relation in relations {
                if relation.get("type").and_then(Value::as_str) != Some("manga") {
                    continue;
                }
                manga_id = relation
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if relation.get("attributes").is_some() {
                    manga_obj = Some(relation.clone());
                }
                break;
            }
        }
        if let Some(manga_obj) = manga_obj {
            return Ok((
                parse_manga(&manga_obj, lang).map_err(|message| Error::new(message, None))?,
                chapter,
            ));
        }
        let Some(manga_id) = manga_id else {
            return Err(Error::new("у главы нет связанного тайтла", None));
        };
        Ok((self.get_manga(&manga_id, lang)?, chapter))
    }

    pub fn at_home(&self, chapter_id: &str) -> Result<Value, Error> {
        let payload = self.get_json(&format!(
            "{API}/at-home/server/{chapter_id}?forcePort443=true"
        ))?;
        let chapter = payload.get("chapter").cloned().unwrap_or(Value::Null);
        if payload.get("baseUrl").and_then(Value::as_str).is_none()
            || chapter.get("hash").and_then(Value::as_str).is_none()
        {
            return Err(Error::new("MangaDex не выдал сервер для страниц", None));
        }
        Ok(payload)
    }

    pub fn fetch_image(
        &self,
        url: &str,
        cancel: &AtomicBool,
    ) -> Result<(bool, Vec<u8>, bool, u128), Fail> {
        self.image_limiter.wait();
        let started = Instant::now();
        let response = self
            .http
            .get(url)
            .header("Referer", "https://mangadex.org/")
            .header("Accept", "image/avif,image/webp,image/png,image/jpeg,*/*")
            .timeout(Duration::from_secs(120))
            .send()
            .map_err(|error| Error::new(format!("сеть: {error}"), None))?;
        let status = response.status();
        let cached = response
            .headers()
            .get("x-cache")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_ascii_uppercase().starts_with("HIT"))
            .unwrap_or(false);
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let mut reader = response;
        let mut data = Vec::new();
        let mut chunk = [0u8; 64 * 1024];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Fail::Cancelled);
            }
            let read = reader
                .read(&mut chunk)
                .map_err(|error| Error::new(format!("сеть: {error}"), None))?;
            if read == 0 {
                break;
            }
            data.extend_from_slice(&chunk[..read]);
        }
        let elapsed = started.elapsed().as_millis();
        let ok = status.as_u16() == 200 && looks_like_image(&data, &content_type);
        Ok((ok, data, cached, elapsed))
    }

    pub fn report(&self, url: &str, success: bool, nbytes: usize, duration: u128, cached: bool) {
        let host = url::Url::parse(url)
            .map(|item| item.host_str().unwrap_or("").to_ascii_lowercase())
            .unwrap_or_default();
        if host.contains("mangadex.org") {
            return;
        }
        let payload = serde_json::json!({
            "url": url,
            "success": success,
            "cached": cached,
            "bytes": nbytes,
            "duration": duration,
        });
        let _ = self
            .http
            .post(REPORT_URL)
            .json(&payload)
            .timeout(Duration::from_secs(15))
            .send();
    }

    fn get_json(&self, url: &str) -> Result<Value, Error> {
        let body = self.request(url, 4)?;
        let payload: Value =
            serde_json::from_slice(&body).map_err(|_| Error::new("ответ не JSON", None))?;
        if payload.get("result").and_then(Value::as_str) == Some("error") {
            return Err(Error::new(error_detail(&payload), None));
        }
        if !payload.is_object() {
            return Err(Error::new("неожиданный ответ API", None));
        }
        Ok(payload)
    }

    fn request(&self, url: &str, retries: usize) -> Result<Vec<u8>, Error> {
        let mut last = Error::new("сеть", None);
        for attempt in 0..retries {
            self.api_limiter.wait();
            let response = match self.http.get(url).timeout(Duration::from_secs(60)).send() {
                Ok(response) => response,
                Err(error) => {
                    last = Error::new(format!("сеть: {error}"), None);
                    if attempt + 1 < retries {
                        thread::sleep(Duration::from_millis(1500 * (attempt as u64 + 1)));
                        continue;
                    }
                    return Err(last);
                }
            };
            let status = response.status().as_u16();
            if matches!(status, 429 | 500 | 502 | 503 | 504) && attempt + 1 < retries {
                let delay = retry_delay(response.headers(), attempt);
                thread::sleep(delay);
                last = Error::new(format!("HTTP {status}"), Some(status));
                continue;
            }
            let headers_status = status;
            let bytes = response
                .bytes()
                .map_err(|error| Error::new(format!("сеть: {error}"), None))?;
            if !(200..300).contains(&headers_status) {
                let detail = serde_json::from_slice::<Value>(&bytes)
                    .ok()
                    .map(|payload| error_detail(&payload))
                    .unwrap_or_else(|| {
                        String::from_utf8_lossy(&bytes[..bytes.len().min(200)])
                            .trim()
                            .to_string()
                    });
                let detail = if detail.is_empty() {
                    "ошибка API MangaDex".to_string()
                } else {
                    detail
                };
                return Err(Error::new(
                    format!("HTTP {headers_status}: {detail}"),
                    Some(headers_status),
                ));
            }
            return Ok(bytes.to_vec());
        }
        Err(last)
    }
}

fn query(pairs: &[(impl AsRef<str>, impl AsRef<str>)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in pairs {
        serializer.append_pair(key.as_ref(), value.as_ref());
    }
    serializer.finish()
}

fn query_str(pairs: &[(String, String)]) -> String {
    query(pairs)
}

fn error_detail(payload: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(errors) = payload.get("errors").and_then(Value::as_array) {
        for error in errors {
            if let Some(detail) = error.get("detail").and_then(Value::as_str) {
                parts.push(detail.to_string());
            }
        }
    }
    if parts.is_empty() {
        "ошибка API MangaDex".to_string()
    } else {
        parts.join("; ")
    }
}

fn retry_delay(headers: &reqwest::header::HeaderMap, attempt: usize) -> Duration {
    if let Some(raw) = headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
    {
        if let Ok(seconds) = raw.parse::<f64>() {
            return Duration::from_secs_f64(seconds.min(30.0));
        }
    }
    Duration::from_secs_f64((0.8 * 2f64.powi(attempt as i32)).min(8.0))
}

pub fn quote_path(name: &str) -> String {
    let mut out = String::new();
    for byte in name.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub fn write_atomic(target: &std::path::Path, data: &[u8]) -> Result<(), Error> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|error| Error::new(error.to_string(), None))?;
    }
    let extension = target
        .extension()
        .and_then(|item| item.to_str())
        .unwrap_or("img");
    let partial = target.with_extension(format!("{extension}.part"));
    {
        let mut file =
            std::fs::File::create(&partial).map_err(|error| Error::new(error.to_string(), None))?;
        file.write_all(data)
            .map_err(|error| Error::new(error.to_string(), None))?;
    }
    std::fs::rename(&partial, target).map_err(|error| Error::new(error.to_string(), None))?;
    Ok(())
}
