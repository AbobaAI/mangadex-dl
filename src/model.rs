use std::collections::{HashMap, HashSet};

use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

const RESERVED_WINDOWS: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

#[derive(Clone, Debug)]
pub struct Manga {
    pub id: String,
    pub title: String,
    pub original_title: String,
    pub year: Option<i32>,
    pub status: Option<String>,
    pub original_language: Option<String>,
    pub languages: Vec<String>,
    pub aliases: Vec<String>,
    pub cover: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Chapter {
    pub id: String,
    pub number: Option<String>,
    pub volume: Option<String>,
    pub title: Option<String>,
    pub language: String,
    pub pages: u32,
    pub group: String,
    pub updated_at: String,
    pub external_url: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DownloadResult {
    pub downloaded: u32,
    pub skipped: u32,
    pub failed: u32,
}

pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn sanitize(name: &str, limit: usize) -> String {
    let normalized: String = name.nfc().collect();
    let mut spaced = String::new();
    for ch in normalized.chars() {
        if ch.is_control() || matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
            spaced.push(' ');
        } else {
            spaced.push(ch);
        }
    }
    let collapsed = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut cleaned = collapsed
        .trim_matches(|ch: char| ch == ' ' || ch == '.')
        .to_string();
    if cleaned.is_empty() {
        cleaned = "untitled".to_string();
    }
    let stem = cleaned.split('.').next().unwrap_or("");
    if RESERVED_WINDOWS.contains(&stem.to_ascii_uppercase().as_str()) {
        cleaned = format!("_{cleaned}");
    }
    if cleaned.chars().count() > limit {
        cleaned = cleaned.chars().take(limit).collect();
        cleaned = cleaned.trim_end_matches([' ', '.']).to_string();
    }
    if cleaned.is_empty() {
        "untitled".to_string()
    } else {
        cleaned
    }
}

fn decimal_places(value: &str) -> usize {
    value
        .split_once('.')
        .map(|(_, frac)| frac.trim_end_matches('0').len())
        .unwrap_or(0)
}

pub fn same_number(spec: &str, value: Option<&str>) -> bool {
    let spec = spec.trim();
    let Some(value) = value.map(str::trim).filter(|item| !item.is_empty()) else {
        return matches!(spec.to_lowercase().as_str(), "oneshot" | "ваншот");
    };
    if spec == value {
        return true;
    }
    let Ok(left) = spec.parse::<f64>() else {
        return false;
    };
    let Ok(right) = value.parse::<f64>() else {
        return false;
    };
    left == right && decimal_places(spec) == decimal_places(value)
}

fn num_key(value: Option<&str>) -> (u8, f64, String) {
    let Some(text) = value.map(str::trim).filter(|item| !item.is_empty()) else {
        return (0, f64::NEG_INFINITY, String::new());
    };
    match text.parse::<f64>() {
        Ok(number) => (0, number, text.to_string()),
        Err(_) => (1, 0.0, text.to_string()),
    }
}

pub fn sort_chapters(mut chapters: Vec<Chapter>) -> Vec<Chapter> {
    chapters.sort_by(|left, right| {
        num_key(left.volume.as_deref())
            .partial_cmp(&num_key(right.volume.as_deref()))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                num_key(left.number.as_deref())
                    .partial_cmp(&num_key(right.number.as_deref()))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| left.updated_at.cmp(&right.updated_at))
            .then_with(|| left.id.cmp(&right.id))
    });
    chapters
}

pub fn latest_per_slot(chapters: &[Chapter]) -> Vec<Chapter> {
    let mut best: HashMap<(String, String), Chapter> = HashMap::new();
    let mut order = Vec::new();
    for chapter in chapters {
        let key = (
            chapter.volume.clone().unwrap_or_default(),
            chapter.number.clone().unwrap_or_default(),
        );
        match best.get(&key) {
            None => {
                order.push(key.clone());
                best.insert(key, chapter.clone());
            }
            Some(current) if chapter.updated_at >= current.updated_at => {
                best.insert(key, chapter.clone());
            }
            Some(_) => {}
        }
    }
    order
        .into_iter()
        .filter_map(|key| best.remove(&key))
        .collect()
}

fn split_range(part: &str) -> Result<Option<(&str, &str)>, String> {
    if part.chars().filter(|ch| *ch == '-').count() != 1 {
        return Ok(None);
    }
    let Some((left, right)) = part.split_once('-') else {
        return Ok(None);
    };
    let left = left.trim();
    let right = right.trim();
    if left.is_empty() || right.is_empty() {
        return Ok(None);
    }
    let left_value = left
        .parse::<f64>()
        .map_err(|_| ())
        .ok()
        .zip(right.parse::<f64>().ok());
    let Some((left_value, right_value)) = left_value else {
        return Ok(None);
    };
    if left_value > right_value {
        return Err(format!("диапазон {part} перевёрнут"));
    }
    Ok(Some((left, right)))
}

fn in_numeric_range(number: &str, left: &str, right: &str) -> bool {
    let Ok(value) = number.parse::<f64>() else {
        return false;
    };
    let (Ok(left), Ok(right)) = (left.parse::<f64>(), right.parse::<f64>()) else {
        return false;
    };
    left <= value && value <= right
}

fn chapter_by_index(chapters: &[Chapter], part: &str) -> Result<Chapter, String> {
    let raw = part[1..].trim();
    if raw.is_empty() || !raw.chars().all(|ch| ch.is_ascii_digit()) {
        return Err(format!("неверный номер строки {part}"));
    }
    let index: usize = raw
        .parse()
        .map_err(|_| format!("неверный номер строки {part}"))?;
    if index < 1 || index > chapters.len() {
        return Err(format!("нет строки {part}, в списке {}", chapters.len()));
    }
    Ok(chapters[index - 1].clone())
}

pub fn select_chapters(chapters: &[Chapter], spec: &str) -> Result<Vec<Chapter>, String> {
    let text = spec.trim();
    if text.is_empty() {
        return Err("пустой номер главы".to_string());
    }
    if matches!(text.to_lowercase().as_str(), "all" | "*" | "все") {
        return Ok(latest_per_slot(chapters));
    }

    let mut picked = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |items: Vec<Chapter>| {
        for chapter in items {
            if seen.insert(chapter.id.clone()) {
                picked.push(chapter);
            }
        }
    };

    for raw_part in text.split(',') {
        let part = raw_part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(stripped) = part.strip_prefix('#') {
            add(vec![chapter_by_index(chapters, &format!("#{stripped}"))?]);
            continue;
        }
        if let Some((left, right)) = split_range(part)? {
            let matched: Vec<Chapter> = chapters
                .iter()
                .filter(|chapter| {
                    chapter
                        .number
                        .as_deref()
                        .is_some_and(|number| in_numeric_range(number, left, right))
                })
                .cloned()
                .collect();
            if matched.is_empty() {
                return Err(format!("в диапазоне {part} нет глав"));
            }
            add(latest_per_slot(&matched));
            continue;
        }
        let matched: Vec<Chapter> = chapters
            .iter()
            .filter(|chapter| same_number(part, chapter.number.as_deref()))
            .cloned()
            .collect();
        if matched.is_empty() {
            return Err(format!("глава {part} не найдена"));
        }
        add(latest_per_slot(&matched));
    }
    if picked.is_empty() {
        return Err("ни одна глава не выбрана".to_string());
    }
    Ok(picked)
}

pub fn filter_volume(chapters: &[Chapter], volume: Option<&str>) -> Result<Vec<Chapter>, String> {
    let Some(volume) = volume.map(str::trim).filter(|item| !item.is_empty()) else {
        return Ok(chapters.to_vec());
    };
    let matched: Vec<Chapter> = chapters
        .iter()
        .filter(|chapter| same_number(volume, chapter.volume.as_deref()))
        .cloned()
        .collect();
    if matched.is_empty() {
        return Err(format!("том {volume} не найден"));
    }
    Ok(matched)
}

fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    let groups = [8, 4, 4, 4, 12];
    let mut index = 0;
    for (group, length) in groups.into_iter().enumerate() {
        if group > 0 {
            if bytes[index] != b'-' {
                return false;
            }
            index += 1;
        }
        for _ in 0..length {
            if !bytes[index].is_ascii_hexdigit() {
                return false;
            }
            index += 1;
        }
    }
    index == bytes.len()
}

fn uuid_after(text: &str, marker: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let position = lower.find(&marker.to_ascii_lowercase())?;
    let rest = text.get(position + marker.len()..)?;
    let candidate: String = rest.chars().take(36).collect();
    is_uuid(&candidate).then(|| candidate.to_ascii_lowercase())
}

pub fn classify_target(text: &str) -> (&'static str, String) {
    let raw = text.trim();
    if let Some(id) = uuid_after(raw, "mangadex.org/chapter/") {
        return ("chapter", id);
    }
    if let Some(id) = uuid_after(raw, "mangadex.org/title/") {
        return ("manga", id);
    }
    if is_uuid(raw) {
        return ("uuid", raw.to_ascii_lowercase());
    }
    ("search", raw.to_string())
}

pub fn validate_lang(lang: &str) -> Result<String, String> {
    let cleaned = lang.trim().to_lowercase().replace('_', "-");
    let mut parts = cleaned.split('-');
    let head = parts.next().unwrap_or("");
    let tail = parts.next();
    let extra = parts.next().is_some();
    let ok = (2..=3).contains(&head.len())
        && head.chars().all(|ch| ch.is_ascii_alphabetic())
        && !extra
        && tail
            .map(|item| {
                (2..=8).contains(&item.len()) && item.chars().all(|ch| ch.is_ascii_alphabetic())
            })
            .unwrap_or(true);
    if ok && !cleaned.is_empty() {
        Ok(cleaned)
    } else {
        Err(format!("неизвестный код языка: {lang}"))
    }
}

fn string_field(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn collect_titles(attributes: &Value) -> HashMap<String, String> {
    let mut titles = HashMap::new();
    if let Some(map) = attributes.get("title").and_then(Value::as_object) {
        for (lang, value) in map {
            if let Some(text) = string_field(value) {
                titles.insert(lang.clone(), text);
            }
        }
    }
    if let Some(alts) = attributes.get("altTitles").and_then(Value::as_array) {
        for alt in alts {
            let Some(map) = alt.as_object() else { continue };
            for (lang, value) in map {
                if let Some(text) = string_field(value) {
                    titles.entry(lang.clone()).or_insert(text);
                }
            }
        }
    }
    titles
}

pub fn choose_title(titles: &HashMap<String, String>, lang: &str) -> String {
    for key in [lang, "en", "ja-ro", "ja", "ko", "zh"] {
        if let Some(title) = titles.get(key).filter(|item| !item.is_empty()) {
            return title.clone();
        }
    }
    titles
        .values()
        .next()
        .cloned()
        .unwrap_or_else(|| "manga".to_string())
}

fn all_aliases(attributes: &Value) -> Vec<String> {
    let mut found = Vec::new();
    if let Some(map) = attributes.get("title").and_then(Value::as_object) {
        for value in map.values() {
            if let Some(text) = string_field(value) {
                found.push(text);
            }
        }
    }
    if let Some(alts) = attributes.get("altTitles").and_then(Value::as_array) {
        for alt in alts {
            let Some(map) = alt.as_object() else { continue };
            for value in map.values() {
                if let Some(text) = string_field(value) {
                    found.push(text);
                }
            }
        }
    }
    found
}

fn cover_url(obj: &Value) -> Option<String> {
    let manga_id = obj.get("id").and_then(Value::as_str)?;
    let relations = obj.get("relationships").and_then(Value::as_array)?;
    for relation in relations {
        if relation.get("type").and_then(Value::as_str) != Some("cover_art") {
            continue;
        }
        if let Some(filename) = relation
            .pointer("/attributes/fileName")
            .and_then(Value::as_str)
        {
            return Some(format!(
                "https://uploads.mangadex.org/covers/{manga_id}/{filename}.256.jpg"
            ));
        }
    }
    None
}

pub fn parse_manga(obj: &Value, lang: &str) -> Result<Manga, String> {
    let attributes = obj.get("attributes").cloned().unwrap_or(Value::Null);
    let titles = collect_titles(&attributes);
    let primary = attributes
        .get("title")
        .and_then(Value::as_object)
        .and_then(|map| map.values().find_map(string_field))
        .unwrap_or_else(|| "manga".to_string());
    let languages = attributes
        .get("availableTranslatedLanguages")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let year = attributes
        .get("year")
        .and_then(Value::as_i64)
        .map(|value| value as i32);
    Ok(Manga {
        id: obj
            .get("id")
            .and_then(Value::as_str)
            .ok_or("у тайтла нет id")?
            .to_string(),
        title: one_line(&choose_title(&titles, lang)),
        original_title: one_line(&primary),
        year,
        status: attributes
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_string),
        original_language: attributes
            .get("originalLanguage")
            .and_then(Value::as_str)
            .map(str::to_string),
        languages,
        aliases: all_aliases(&attributes),
        cover: cover_url(obj),
    })
}

pub fn parse_chapter(obj: &Value) -> Result<Chapter, String> {
    let attributes = obj.get("attributes").cloned().unwrap_or(Value::Null);
    let mut groups = Vec::new();
    let mut uploader = String::new();
    if let Some(relations) = obj.get("relationships").and_then(Value::as_array) {
        for relation in relations {
            let kind = relation.get("type").and_then(Value::as_str).unwrap_or("");
            let attrs = relation.get("attributes").cloned().unwrap_or(Value::Null);
            if kind == "scanlation_group" {
                if let Some(name) = attrs.get("name").and_then(string_field) {
                    groups.push(name);
                }
            } else if kind == "user" && uploader.is_empty() {
                if let Some(name) = attrs.get("username").and_then(string_field) {
                    uploader = name;
                }
            }
        }
    }
    let number = attributes.get("chapter").and_then(string_field);
    let volume = attributes.get("volume").and_then(string_field);
    let title = attributes.get("title").and_then(string_field);
    let credit = if groups.is_empty() {
        uploader
    } else {
        groups.join(", ")
    };
    Ok(Chapter {
        id: obj
            .get("id")
            .and_then(Value::as_str)
            .ok_or("у главы нет id")?
            .to_string(),
        number,
        volume,
        title: title.as_deref().map(one_line),
        language: attributes
            .get("translatedLanguage")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        pages: attributes.get("pages").and_then(Value::as_u64).unwrap_or(0) as u32,
        group: if credit.is_empty() {
            String::new()
        } else {
            one_line(&credit)
        },
        updated_at: attributes
            .get("updatedAt")
            .and_then(Value::as_str)
            .or_else(|| attributes.get("publishAt").and_then(Value::as_str))
            .unwrap_or("")
            .to_string(),
        external_url: attributes.get("externalUrl").and_then(string_field),
    })
}

pub fn credit(chapter: &Chapter) -> String {
    if chapter.group.is_empty() {
        "без группы".to_string()
    } else {
        chapter.group.clone()
    }
}

pub fn chapter_dirname(chapter: &Chapter) -> String {
    let number = chapter.number.as_deref().unwrap_or("").trim();
    let mut label = if !number.is_empty() && number.chars().all(|ch| ch.is_ascii_digit()) {
        format!("c{:03}", number.parse::<u32>().unwrap_or(0))
    } else if !number.is_empty() {
        format!("c{}", number.replace('/', "-"))
    } else {
        "c000-oneshot".to_string()
    };
    if let Some(volume) = chapter.volume.as_deref().filter(|item| !item.is_empty()) {
        label = format!("v{volume}-{label}");
    }
    if !chapter.group.is_empty() {
        label.push_str(&format!(" [{}]", chapter.group));
    }
    if let Some(title) = chapter.title.as_deref().filter(|item| !item.is_empty()) {
        label.push_str(&format!(" - {title}"));
    }
    sanitize(&label, 80)
}

pub fn status_label(status: Option<&str>) -> &str {
    match status {
        Some("ongoing") => "выходит",
        Some("completed") => "завершён",
        Some("hiatus") => "пауза",
        Some("cancelled") => "отменён",
        Some(other) => other,
        None => "—",
    }
}

pub fn looks_like_image(data: &[u8], content_type: &str) -> bool {
    if data.len() < 16 {
        return false;
    }
    if data.starts_with(b"\x89PNG")
        || data.starts_with(b"\xff\xd8\xff")
        || data.starts_with(b"GIF8")
        || data.starts_with(b"RIFF")
    {
        return true;
    }
    if data.len() >= 32 && data[..32].windows(4).any(|window| window == b"ftyp") {
        return true;
    }
    content_type.to_ascii_lowercase().starts_with("image/")
        && !data.starts_with(b"<")
        && !data.starts_with(b"{")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chapter(
        id: &str,
        number: Option<&str>,
        volume: Option<&str>,
        group: &str,
        updated: &str,
    ) -> Chapter {
        Chapter {
            id: id.to_string(),
            number: number.map(str::to_string),
            volume: volume.map(str::to_string),
            title: None,
            language: "ru".to_string(),
            pages: 10,
            group: group.to_string(),
            updated_at: updated.to_string(),
            external_url: None,
        }
    }

    fn sample() -> Vec<Chapter> {
        vec![
            chapter("a", Some("1"), None, "Old", "2020-01-01T00:00:00+00:00"),
            chapter("b", Some("1"), None, "New", "2024-01-01T00:00:00+00:00"),
            chapter(
                "c",
                Some("2"),
                Some("1"),
                "Vol",
                "2020-01-01T00:00:00+00:00",
            ),
            chapter("d", Some("12.5"), None, "Half", "2020-01-01T00:00:00+00:00"),
            chapter("e", None, None, "Oneshot", "2020-01-01T00:00:00+00:00"),
        ]
    }

    #[test]
    fn strips_windows_illegal_characters() {
        assert_eq!(sanitize(r#"a<b>:c/d\e|f?g*h""#, 60), "a b c d e f g h");
    }

    #[test]
    fn reserves_windows_device_names() {
        assert_eq!(sanitize("CON", 60), "_CON");
    }

    #[test]
    fn trims_trailing_dot_and_length() {
        assert_eq!(sanitize("name.", 60), "name");
        assert_eq!(sanitize(&"я".repeat(80), 12).chars().count(), 12);
    }

    #[test]
    fn classifies_title_chapter_and_search() {
        let title = "https://mangadex.org/title/58be6aa6-06cb-4ca5-bd20-f1392ce451fb/yotsuba";
        let chapter_url = "https://mangadex.org/chapter/abb708e6-66cc-4a77-8255-a42008da2f70";
        assert_eq!(classify_target(title).0, "manga");
        assert_eq!(classify_target(chapter_url).0, "chapter");
        assert_eq!(classify_target("Ёцуба"), ("search", "Ёцуба".to_string()));
        assert_eq!(
            classify_target("58be6aa6-06cb-4ca5-bd20-f1392ce451fb").0,
            "uuid"
        );
    }

    #[test]
    fn prefers_requested_language() {
        let titles = HashMap::from([
            ("ja-ro".to_string(), "Yotsuba to!".to_string()),
            ("en".to_string(), "Yotsuba&!".to_string()),
            ("ru".to_string(), "Ёцуба!".to_string()),
        ]);
        assert_eq!(choose_title(&titles, "ru"), "Ёцуба!");
        assert_eq!(choose_title(&titles, "en"), "Yotsuba&!");
    }

    #[test]
    fn single_number_picks_latest_group() {
        let picked = select_chapters(&sample(), "1").unwrap();
        assert_eq!(
            picked
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["b"]
        );
    }

    #[test]
    fn decimal_does_not_match_integer() {
        assert!(!same_number("12", Some("12.5")));
        assert!(same_number("12.50", Some("12.5")));
        assert!(select_chapters(&sample(), "12").is_err());
    }

    #[test]
    fn range_includes_decimal_and_keeps_volumes() {
        let picked = select_chapters(&sample(), "1-12.5").unwrap();
        assert_eq!(
            picked
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["b", "c", "d"]
        );
    }

    #[test]
    fn list_oneshot_and_index() {
        let picked = select_chapters(&sample(), "2, oneshot").unwrap();
        assert_eq!(
            picked
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["c", "e"]
        );
        assert_eq!(select_chapters(&sample(), "#1").unwrap()[0].id, "a");
    }

    #[test]
    fn all_collapses_duplicate_groups_only() {
        let picked = latest_per_slot(&sample());
        assert_eq!(
            picked
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["b", "c", "d", "e"]
        );
    }

    #[test]
    fn uploader_is_used_when_there_is_no_group() {
        let parsed = parse_chapter(&serde_json::json!({
            "id": "x",
            "attributes": {"chapter": "1", "pages": 2, "translatedLanguage": "ru"},
            "relationships": [{"type": "user", "attributes": {"username": "az228"}}]
        }))
        .unwrap();
        assert_eq!(parsed.group, "az228");
        let mut item = chapter("id", Some("4"), None, "", "2020-01-01T00:00:00+00:00");
        item.group.clear();
        assert_eq!(chapter_dirname(&item), "c004");
    }

    #[test]
    fn folder_name_is_padded_and_safe() {
        let mut item = chapter(
            "id",
            Some("3"),
            Some("2"),
            "A/B",
            "2020-01-01T00:00:00+00:00",
        );
        item.title = Some("Том: \"старт\"".to_string());
        let name = chapter_dirname(&item);
        assert!(name.starts_with("v2-c003"), "{name}");
        assert!(!name.contains('/'));
        assert!(!name.contains(':'));
    }
}
