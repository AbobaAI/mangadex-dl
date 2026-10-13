use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

use crate::client::{quote_path, write_atomic, Client, Fail};
use crate::model::{chapter_dirname, sanitize, Chapter, DownloadResult, Manga};

pub fn chapter_dir(root: &Path, manga: &Manga, lang: &str, chapter: &Chapter) -> PathBuf {
    root.join(sanitize(&manga.title, 60))
        .join(sanitize(lang, 16))
        .join(chapter_dirname(chapter))
}

pub fn download_chapter(
    client: &Client,
    manga: &Manga,
    lang: &str,
    chapter: &Chapter,
    root: &Path,
    saver: bool,
    force: bool,
    cancel: &AtomicBool,
    mut on_page: Option<&mut dyn FnMut(u32, u32, &str)>,
) -> Result<DownloadResult, Fail> {
    if cancel.load(Ordering::Relaxed) {
        return Err(Fail::Cancelled);
    }
    if chapter.external_url.is_some() || chapter.pages == 0 {
        return Ok(DownloadResult {
            failed: 1,
            ..DownloadResult::default()
        });
    }
    let destination = chapter_dir(root, manga, lang, chapter);
    std::fs::create_dir_all(&destination)?;
    let quality = if saver { "data-saver" } else { "data" };
    let file_key = if saver { "dataSaver" } else { "data" };
    let mut server = client.at_home(&chapter.id)?;
    let mut filenames = filenames_of(&server, file_key);
    if filenames.is_empty() {
        return Ok(DownloadResult {
            failed: 1,
            ..DownloadResult::default()
        });
    }
    let total = filenames.len() as u32;
    let mut result = DownloadResult::default();
    for index in 1..=total {
        if cancel.load(Ordering::Relaxed) {
            return Err(Fail::Cancelled);
        }
        let mut filename = filenames[(index - 1) as usize].clone();
        let suffix = Path::new(&filename)
            .extension()
            .and_then(|item| item.to_str())
            .map(|item| format!(".{item}"))
            .unwrap_or_else(|| ".img".to_string());
        let target = destination.join(format!("{index:03}{suffix}"));
        if !force
            && target.is_file()
            && target
                .metadata()
                .map(|meta| meta.len() > 0)
                .unwrap_or(false)
        {
            result.skipped += 1;
            if let Some(callback) = on_page.as_mut() {
                callback(index, total, "skipped");
            }
            continue;
        }
        let mut saved = false;
        for _attempt in 0..3 {
            if cancel.load(Ordering::Relaxed) {
                return Err(Fail::Cancelled);
            }
            let base = server
                .get("baseUrl")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim_end_matches('/');
            let hash = server
                .pointer("/chapter/hash")
                .and_then(Value::as_str)
                .unwrap_or("");
            let url = format!("{base}/{quality}/{hash}/{}", quote_path(&filename));
            let (ok, data, cached, elapsed) = client.fetch_image(&url, cancel)?;
            client.report(&url, ok, data.len(), elapsed, cached);
            if ok {
                write_atomic(&target, &data)?;
                result.downloaded += 1;
                saved = true;
                if let Some(callback) = on_page.as_mut() {
                    callback(index, total, "saved");
                }
                break;
            }
            server = client.at_home(&chapter.id)?;
            let refreshed = filenames_of(&server, file_key);
            if refreshed.len() == total as usize {
                filename = refreshed[(index - 1) as usize].clone();
                filenames = refreshed;
            }
        }
        if !saved {
            result.failed += 1;
            if let Some(callback) = on_page.as_mut() {
                callback(index, total, "failed");
            }
        }
    }
    Ok(result)
}

fn filenames_of(server: &Value, file_key: &str) -> Vec<String> {
    server
        .pointer(&format!("/chapter/{file_key}"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

impl From<std::io::Error> for Fail {
    fn from(error: std::io::Error) -> Self {
        Fail::Api(crate::client::Error {
            message: error.to_string(),
            status: None,
        })
    }
}
