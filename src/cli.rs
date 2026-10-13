use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use clap::Parser;

use crate::client::{Client, Error, Fail};
use crate::download::download_chapter;
use crate::model::{
    classify_target, credit, filter_volume, select_chapters, validate_lang, Chapter,
    DownloadResult, Manga,
};

macro_rules! say {
    ($($token:tt)*) => {
        write_line(format_args!($($token)*))
    };
}

#[derive(Parser, Debug)]
#[command(
    name = "mangadex-dl",
    version,
    about = "Скачивает страницы выбранных глав с mangadex.org для личного некоммерческого чтения.",
    after_help = "Без аргументов открывается окно программы.\n--cli спрашивает название и главы в терминале.\nГлава: 12 | 1-5 | 1,3,8 | all | #2.\nПовторный запуск пропускает уже скачанные страницы."
)]
pub struct Args {
    /// Название, ссылка на тайтл или ссылка на главу
    pub target: Option<String>,
    /// Какие главы скачать
    #[arg(short, long)]
    pub chapter: Option<String>,
    /// Язык перевода, по умолчанию ru
    #[arg(short, long, default_value = "ru")]
    pub lang: String,
    /// Только этот том
    #[arg(short = 'v', long)]
    pub volume: Option<String>,
    /// Папка для страниц
    #[arg(short, long, default_value = "downloads")]
    pub out: PathBuf,
    /// Сжатые страницы
    #[arg(long)]
    pub saver: bool,
    /// Показать главы и выйти
    #[arg(long)]
    pub list: bool,
    /// Скачать страницы заново
    #[arg(long)]
    pub force: bool,
    /// Номер тайтла в результатах поиска, с 1
    #[arg(long)]
    pub pick: Option<usize>,
    /// Спрашивать название и главы в терминале
    #[arg(long)]
    pub cli: bool,
}

impl Args {
    pub fn wants_window(&self) -> bool {
        !self.cli
            && self.target.is_none()
            && self.chapter.is_none()
            && self.volume.is_none()
            && self.pick.is_none()
            && !self.list
    }
}

pub fn execute(args: Args) -> i32 {
    match run(args) {
        Ok(code) => code,
        Err(Fail::Pipe) => 0,
        Err(Fail::Cancelled) => {
            eprintln!("Остановлено");
            130
        }
        Err(Fail::Api(error)) => {
            eprintln!("Ошибка: {error}");
            1
        }
        Err(Fail::Message(message)) => {
            eprintln!("Ошибка: {message}");
            1
        }
    }
}

fn run(args: Args) -> Result<i32, Fail> {
    let mut lang = validate_lang(&args.lang)?;
    let client = Client::new()?;
    let mut target = args.target.clone().unwrap_or_default();
    target = target.trim().to_string();
    if target.is_empty() {
        if !is_tty() {
            return Err(Fail::Message("укажите название или ссылку MangaDex".into()));
        }
        target = prompt("Название или ссылка MangaDex: ");
        if target.is_empty() {
            return Err(Fail::Message("пустой запрос".into()));
        }
    }
    let (kind, value) = classify_target(&target);
    let mut direct = None;
    let manga = if kind == "chapter" {
        let (manga, chapter, use_lang) = direct_chapter(&client, &value, &lang)?;
        lang = use_lang;
        direct = Some(vec![chapter]);
        manga
    } else if kind == "manga" {
        client.get_manga(&value, &lang)?
    } else if kind == "uuid" {
        match client.get_manga(&value, &lang) {
            Ok(manga) => manga,
            Err(error) if error.status == Some(404) => {
                let (manga, chapter, use_lang) = direct_chapter(&client, &value, &lang)?;
                lang = use_lang;
                direct = Some(vec![chapter]);
                manga
            }
            Err(error) => return Err(error.into()),
        }
    } else {
        choose_manga(&client, &value, &lang, args.pick)?
    };

    say!("Тайтл: {}", manga.title)?;
    say!("Страница: https://mangadex.org/title/{}", manga.id)?;

    let selected = if let Some(chapters) = direct {
        if args.list {
            print_chapters(&chapters)?;
            return Ok(0);
        }
        chapters
    } else {
        let (use_lang, mut chapters) = load_chapters(&client, &manga, &lang)?;
        lang = use_lang;
        if chapters.is_empty() {
            return Ok(1);
        }
        chapters = filter_volume(&chapters, args.volume.as_deref())?;
        if args.list {
            print_chapters(&chapters)?;
            return Ok(0);
        }
        let mut spec = args.chapter.clone().unwrap_or_default();
        spec = spec.trim().to_string();
        if spec.is_empty() {
            if !is_tty() {
                return Err(Fail::Message(
                    "укажите главы через -c, например -c 12 или -c 1-5".into(),
                ));
            }
            spec = ask_spec(&chapters)?;
        }
        let selected = select_chapters(&chapters, &spec)?;
        note_alternatives(&chapters, &selected, &spec)?;
        selected
    };

    let root = args.out.canonicalize().unwrap_or_else(|_| {
        let mut path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        path.push(&args.out);
        path
    });
    if !root.exists() {
        std::fs::create_dir_all(&root)?;
    }
    let root = root.canonicalize().unwrap_or(root);
    say!("Язык: {lang}")?;
    say!("К сохранению: {}", selected.len())?;
    say!("Папка: {}", root.display())?;
    let mut totals = DownloadResult::default();
    let cancel = AtomicBool::new(false);
    for chapter in &selected {
        let label = chapter.number.as_deref().unwrap_or("oneshot");
        say!("Скачиваю главу {label} [{}]", credit(chapter))?;
        let tty = io::stdout().is_terminal();
        let mut callback = |index: u32, total: u32, state: &str| {
            if state == "failed" {
                let _ = writeln!(
                    io::stdout(),
                    "\nГлава {label}, страница {index}: не скачалась"
                );
                return;
            }
            let line = if state == "skipped" {
                format!("  {index}/{total} уже есть")
            } else {
                format!("  {index}/{total} {index:03}")
            };
            if tty {
                let _ = write!(io::stdout(), "\r{line}          ");
                let _ = io::stdout().flush();
            } else {
                let _ = writeln!(io::stdout(), "{line}");
            }
        };
        match download_chapter(
            &client,
            &manga,
            &lang,
            chapter,
            &root,
            args.saver,
            args.force,
            &cancel,
            Some(&mut callback),
        ) {
            Ok(result) => {
                if tty {
                    let _ = writeln!(io::stdout());
                }
                say!(
                    "Глава {label}: новых {}, уже было {}, ошибок {} → {}",
                    result.downloaded,
                    result.skipped,
                    result.failed,
                    crate::download::chapter_dir(&root, &manga, &lang, chapter).display()
                )?;
                totals.downloaded += result.downloaded;
                totals.skipped += result.skipped;
                totals.failed += result.failed;
            }
            Err(Fail::Api(error)) => {
                eprintln!("Глава {label}: {error}");
                totals.failed += 1;
            }
            Err(error) => return Err(error),
        }
    }
    say!(
        "Готово. Новых страниц: {}, пропущено: {}, ошибок: {}",
        totals.downloaded,
        totals.skipped,
        totals.failed
    )?;
    Ok(if totals.failed > 0 { 1 } else { 0 })
}

fn direct_chapter(
    client: &Client,
    id: &str,
    lang: &str,
) -> Result<(Manga, Chapter, String), Error> {
    let (manga, chapter) = client.chapter_with_manga(id, lang)?;
    let use_lang = if chapter.language.is_empty() {
        lang.to_string()
    } else {
        chapter.language.clone()
    };
    Ok((manga, chapter, use_lang))
}

fn choose_manga(
    client: &Client,
    query: &str,
    lang: &str,
    pick: Option<usize>,
) -> Result<Manga, Fail> {
    let (found, total) = client.search(query, lang)?;
    if found.is_empty() {
        return Err(Fail::Message(format!("тайтл «{query}» не найден")));
    }
    if let Some(pick) = pick {
        return found.get(pick.saturating_sub(1)).cloned().ok_or_else(|| {
            Fail::Message(format!(
                "нет тайтла с номером {pick}, в списке {}",
                found.len()
            ))
        });
    }
    let folded = query.to_lowercase();
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
        return Ok(exact[0].clone());
    }
    if found.len() == 1 {
        return Ok(found[0].clone());
    }
    if total > found.len() {
        say!(
            "Показаны первые {} из {total}. Если нужного нет, вставьте ссылку на тайтл.",
            found.len()
        )?;
    }
    say!("Найдено совпадений: {total}. Уточните номер.")?;
    for (index, manga) in found.iter().enumerate() {
        say!("{}", format_manga(index + 1, manga))?;
    }
    if !is_tty() {
        return Err(Fail::Message(
            "несколько тайтлов, укажите --pick N или ссылку на тайтл".into(),
        ));
    }
    let raw = prompt("Номер тайтла: ");
    let Ok(number) = raw.parse::<usize>() else {
        return Err(Fail::Message("нужен номер тайтла из списка".into()));
    };
    found
        .get(number.saturating_sub(1))
        .filter(|_| number >= 1)
        .cloned()
        .ok_or_else(|| Fail::Message(format!("нет тайтла с номером {number}")))
}

fn load_chapters(
    client: &Client,
    manga: &Manga,
    lang: &str,
) -> Result<(String, Vec<Chapter>), Fail> {
    let mut current = lang.to_string();
    loop {
        let chapters = client.chapters(&manga.id, &current)?;
        if !chapters.is_empty() {
            return Ok((current, chapters));
        }
        let available = if manga.languages.is_empty() {
            "нет данных".to_string()
        } else {
            manga.languages.join(", ")
        };
        say!("На языке {current} глав нет. Доступные языки: {available}")?;
        if !is_tty() {
            return Err(Fail::Message(format!("на языке {current} глав нет")));
        }
        let raw = prompt("Другой язык (пусто — выход): ");
        if raw.is_empty() {
            return Ok((current, Vec::new()));
        }
        current = validate_lang(&raw)?;
    }
}

fn ask_spec(chapters: &[Chapter]) -> Result<String, Fail> {
    say!("Глав в списке: {}", chapters.len())?;
    for (index, chapter) in chapters.iter().enumerate() {
        say!("{}", format_chapter(index + 1, chapter))?;
    }
    let raw = prompt("Какие главы скачать? (12, 1-5, 1,3, all, #2): ");
    if raw.is_empty() {
        return Err(Fail::Message("глава не выбрана".into()));
    }
    Ok(raw)
}

fn note_alternatives(chapters: &[Chapter], selected: &[Chapter], spec: &str) -> Result<(), Fail> {
    if spec.contains('#') {
        return Ok(());
    }
    for chapter in selected {
        let siblings: Vec<&Chapter> = chapters
            .iter()
            .filter(|other| {
                other.id != chapter.id
                    && other.volume == chapter.volume
                    && crate::model::same_number(
                        chapter.number.as_deref().unwrap_or("oneshot"),
                        other.number.as_deref(),
                    )
            })
            .collect();
        if siblings.is_empty() {
            continue;
        }
        let others = siblings
            .iter()
            .filter_map(|item| {
                chapters
                    .iter()
                    .position(|candidate| candidate.id == item.id)
                    .map(|index| format!("#{} {}", index + 1, credit(item)))
            })
            .collect::<Vec<_>>()
            .join(", ");
        let label = chapter.number.as_deref().unwrap_or("oneshot");
        say!(
            "Глава {label}: скачиваю самый свежий перевод «{}». Другие: {others}",
            credit(chapter)
        )?;
    }
    Ok(())
}

fn print_chapters(chapters: &[Chapter]) -> Result<(), Fail> {
    say!("Глав: {}", chapters.len())?;
    for (index, chapter) in chapters.iter().enumerate() {
        say!("{}", format_chapter(index + 1, chapter))?;
    }
    Ok(())
}

fn format_manga(index: usize, manga: &Manga) -> String {
    let year = manga
        .year
        .map(|value| value.to_string())
        .unwrap_or_else(|| "—".into());
    let status = manga.status.clone().unwrap_or_else(|| "—".into());
    let origin = manga
        .original_language
        .clone()
        .unwrap_or_else(|| "—".into());
    let extra = if !manga.original_title.is_empty()
        && manga.original_title.to_lowercase() != manga.title.to_lowercase()
    {
        format!(" / {}", manga.original_title)
    } else {
        String::new()
    };
    let langs = if manga.languages.is_empty() {
        "—".to_string()
    } else {
        manga
            .languages
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "{index:3}. {}{extra}  ({year}, {status}, {origin})  [{langs}]\n     https://mangadex.org/title/{}",
        manga.title, manga.id
    )
}

fn format_chapter(index: usize, chapter: &Chapter) -> String {
    let number = chapter.number.as_deref().unwrap_or("oneshot");
    let volume = chapter
        .volume
        .as_deref()
        .map(|value| format!("том {value}"))
        .unwrap_or_else(|| "том —".into());
    let title = chapter
        .title
        .as_deref()
        .map(|value| format!("  {value}"))
        .unwrap_or_default();
    let external = if chapter.external_url.is_some() {
        "  внешняя ссылка"
    } else {
        ""
    };
    format!(
        "{index:4}. глава {number:<8} {volume:<10} {:4} стр.  {}{title}{external}",
        chapter.pages,
        credit(chapter)
    )
}

fn prompt(text: &str) -> String {
    eprint!("{text}");
    let _ = io::stderr().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return String::new();
    }
    line.trim().to_string()
}

fn is_tty() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

fn write_line(args: std::fmt::Arguments<'_>) -> Result<(), Fail> {
    let mut out = io::stdout().lock();
    writeln!(out, "{args}").map_err(pipe_or_message)?;
    out.flush().map_err(pipe_or_message)
}

fn pipe_or_message(error: io::Error) -> Fail {
    if error.kind() == io::ErrorKind::BrokenPipe {
        Fail::Pipe
    } else {
        Fail::Message(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> Args {
        Args {
            target: None,
            chapter: None,
            lang: "ru".to_string(),
            volume: None,
            out: PathBuf::from("downloads"),
            saver: false,
            list: false,
            force: false,
            pick: None,
            cli: false,
        }
    }

    #[test]
    fn no_args_open_the_window() {
        assert!(args().wants_window());
    }

    #[test]
    fn flags_stay_in_the_terminal() {
        let mut sample = args();
        sample.cli = true;
        assert!(!sample.wants_window());
        sample = args();
        sample.target = Some("Ёцуба".to_string());
        assert!(!sample.wants_window());
        sample = args();
        sample.list = true;
        assert!(!sample.wants_window());
    }
}
