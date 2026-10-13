use crate::client::{Client, Error};
use crate::model::{classify_target, validate_lang, Chapter, Manga};

pub struct Opened {
    pub lang: String,
    pub results: Vec<Manga>,
    pub total: usize,
    pub manga: Option<Manga>,
    pub chapters: Vec<Chapter>,
    pub select: Vec<String>,
}

pub fn open_query(client: &Client, query: &str, lang: &str) -> Result<Opened, Error> {
    let cleaned = query.trim();
    if cleaned.is_empty() {
        return Err(Error {
            message: "пустой запрос".to_string(),
            status: None,
        });
    }
    let lang = validate_lang(lang).map_err(|message| Error {
        message,
        status: None,
    })?;
    let (kind, value) = classify_target(cleaned);
    if kind == "chapter" {
        return open_chapter(client, &value, &lang);
    }
    if kind == "manga" || kind == "uuid" {
        match client.get_manga(&value, &lang) {
            Ok(manga) => return finish(client, manga, &lang, Vec::new(), Vec::new(), 1),
            Err(error) if kind == "uuid" && error.status == Some(404) => {
                return open_chapter(client, &value, &lang);
            }
            Err(error) => return Err(error),
        }
    }
    let (found, total) = client.search(&value, &lang)?;
    let folded = value.to_lowercase();
    let exact: Vec<&Manga> = found
        .iter()
        .filter(|manga| {
            manga
                .aliases
                .iter()
                .any(|alias| alias.to_lowercase() == folded)
        })
        .collect();
    if exact.len() == 1 {
        return finish(client, exact[0].clone(), &lang, found, Vec::new(), total);
    }
    if found.len() == 1 {
        return finish(client, found[0].clone(), &lang, found, Vec::new(), total);
    }
    Ok(Opened {
        lang,
        results: found,
        total,
        manga: None,
        chapters: Vec::new(),
        select: Vec::new(),
    })
}

pub fn open_manga(client: &Client, id: &str, lang: &str) -> Result<Opened, Error> {
    let lang = validate_lang(lang).map_err(|message| Error {
        message,
        status: None,
    })?;
    let manga = client.get_manga(id, &lang)?;
    finish(client, manga, &lang, Vec::new(), Vec::new(), 1)
}

fn open_chapter(client: &Client, id: &str, lang: &str) -> Result<Opened, Error> {
    let (mut manga, chapter) = client.chapter_with_manga(id, lang)?;
    let lang = if chapter.language.is_empty() {
        lang.to_string()
    } else {
        validate_lang(&chapter.language).unwrap_or_else(|_| lang.to_string())
    };
    if !chapter.language.is_empty() {
        manga = client.get_manga(&manga.id, &lang)?;
    }
    let select = vec![chapter.id.clone()];
    finish(client, manga, &lang, Vec::new(), select, 1)
}

fn finish(
    client: &Client,
    manga: Manga,
    lang: &str,
    results: Vec<Manga>,
    select: Vec<String>,
    total: usize,
) -> Result<Opened, Error> {
    let chapters = client.chapters(&manga.id, lang)?;
    let (results, total) = shown_results(&manga, results, total);
    Ok(Opened {
        lang: lang.to_string(),
        results,
        total,
        manga: Some(manga),
        chapters,
        select,
    })
}

pub(crate) fn shown_results(
    manga: &Manga,
    results: Vec<Manga>,
    total: usize,
) -> (Vec<Manga>, usize) {
    if results.is_empty() {
        (vec![manga.clone()], 1)
    } else {
        (results, total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manga(id: &str, title: &str) -> Manga {
        Manga {
            id: id.to_string(),
            title: title.to_string(),
            original_title: title.to_string(),
            year: None,
            status: None,
            original_language: None,
            languages: Vec::new(),
            aliases: Vec::new(),
            cover: None,
        }
    }

    #[test]
    fn direct_open_replaces_the_result_list() {
        let title = manga("a", "Один");
        let (results, total) = shown_results(&title, Vec::new(), 32);
        assert_eq!(total, 1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "a");
    }

    #[test]
    fn exact_search_keeps_the_other_hits() {
        let title = manga("b", "Ёцуба!");
        let hits = vec![title.clone(), manga("c", "Другое")];
        let (results, total) = shown_results(&title, hits, 32);
        assert_eq!(total, 32);
        assert_eq!(results.len(), 2);
    }
}
