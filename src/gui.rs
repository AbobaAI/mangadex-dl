use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use eframe::egui;

use crate::client::{Client, Fail};
use crate::download::{chapter_dir, download_chapter};
use crate::model::{credit, select_chapters, status_label, Chapter, Manga};
use crate::session::{self, Opened};

const LANGS: &[(&str, &str)] = &[
    ("ru", "русский"),
    ("en", "english"),
    ("uk", "українська"),
    ("be", "беларуская"),
    ("es", "español"),
    ("es-la", "español (LATAM)"),
    ("pt-br", "português"),
    ("fr", "français"),
    ("de", "deutsch"),
    ("pl", "polski"),
    ("it", "italiano"),
    ("tr", "türkçe"),
    ("id", "indonesia"),
    ("vi", "tiếng việt"),
    ("th", "ไทย"),
    ("zh", "中文"),
    ("zh-hk", "中文（香港）"),
    ("ja", "日本語"),
    ("ko", "한국어"),
];

struct Palette {
    bg: egui::Color32,
    ink: egui::Color32,
    muted: egui::Color32,
    line: egui::Color32,
    paper: egui::Color32,
    track: egui::Color32,
    accent: egui::Color32,
    accent_ink: egui::Color32,
    ok: egui::Color32,
}

fn rgb(value: u32) -> egui::Color32 {
    egui::Color32::from_rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            bg: rgb(0x141210),
            ink: rgb(0xf4efe6),
            muted: rgb(0xb2aa9e),
            line: rgb(0x2e2924),
            paper: rgb(0x221e1a),
            track: rgb(0x3a342c),
            accent: rgb(0xe08a78),
            accent_ink: rgb(0x1a100e),
            ok: rgb(0x8dccab),
        }
    } else {
        Palette {
            bg: rgb(0xf3efe6),
            ink: rgb(0x1c1915),
            muted: rgb(0x5f594f),
            line: rgb(0xddd4c6),
            paper: rgb(0xe7e0d4),
            track: rgb(0xcfc4b4),
            accent: rgb(0x8d3428),
            accent_ink: rgb(0xfff8f4),
            ok: rgb(0x215c40),
        }
    }
}

fn visuals(colors: &Palette, dark: bool) -> egui::Visuals {
    let mut visuals = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.window_fill = colors.bg;
    visuals.panel_fill = colors.bg;
    visuals.faint_bg_color = colors.paper;
    visuals.extreme_bg_color = colors.paper;
    visuals.code_bg_color = colors.paper;
    visuals.override_text_color = Some(colors.ink);
    visuals.hyperlink_color = colors.accent;
    visuals.selection.bg_fill = colors.accent.gamma_multiply(0.25);
    visuals.selection.stroke = egui::Stroke::new(1.0, colors.accent);
    visuals.window_stroke = egui::Stroke::new(1.0, colors.line);
    let stroke = egui::Stroke::new(1.0, colors.line);
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.bg_stroke = stroke;
        widget.fg_stroke.color = colors.ink;
    }
    visuals.widgets.noninteractive.fg_stroke.color = colors.muted;
    visuals.widgets.noninteractive.bg_fill = colors.bg;
    visuals.widgets.inactive.bg_fill = colors.bg;
    visuals.widgets.inactive.weak_bg_fill = colors.bg;
    visuals.widgets.hovered.bg_fill = colors.paper;
    visuals.widgets.hovered.weak_bg_fill = colors.paper;
    visuals.widgets.active.bg_fill = colors.accent;
    visuals.widgets.active.fg_stroke.color = colors.accent_ink;
    visuals
}

enum Command {
    Search {
        gen: u64,
        query: String,
        lang: String,
    },
    Open {
        gen: u64,
        id: String,
        lang: String,
    },
    Download {
        gen: u64,
        manga: Manga,
        lang: String,
        chapters: Vec<Chapter>,
        root: PathBuf,
        saver: bool,
    },
}

enum Event {
    Opened {
        gen: u64,
        opened: Opened,
    },
    Failed {
        gen: u64,
        message: String,
    },
    Cover {
        gen: u64,
        id: String,
        bytes: Vec<u8>,
    },
    Progress {
        gen: u64,
        done_chapters: u32,
        chapter_count: u32,
        page: u32,
        pages: u32,
        message: String,
    },
    Finished {
        gen: u64,
        message: String,
        stopped: bool,
        failed: bool,
        folder: PathBuf,
    },
}

struct Job {
    running: bool,
    stopping: bool,
    stopped: bool,
    failed: bool,
    message: String,
    done_chapters: u32,
    chapter_count: u32,
    page: u32,
    pages: u32,
    folder: Option<PathBuf>,
}

impl Default for Job {
    fn default() -> Self {
        Self {
            running: false,
            stopping: false,
            stopped: false,
            failed: false,
            message: String::new(),
            done_chapters: 0,
            chapter_count: 0,
            page: 0,
            pages: 1,
            folder: None,
        }
    }
}

pub struct App {
    tx: Sender<Command>,
    rx: Receiver<Event>,
    cancel: Arc<AtomicBool>,
    generation: u64,
    query: String,
    focus_query: bool,
    lang: String,
    extra_langs: Vec<String>,
    out: String,
    saver: bool,
    spec: String,
    notice: String,
    results: Vec<Manga>,
    total: usize,
    manga: Option<Manga>,
    chapters: Vec<Chapter>,
    selected: HashSet<String>,
    anchor: usize,
    reveal_title: bool,
    busy: bool,
    job: Job,
    covers: HashMap<String, egui::TextureHandle>,
    theme_dark: Option<bool>,
}

pub fn run(lang: &str, out: &Path, saver: bool) -> Result<(), eframe::Error> {
    let lang = lang.to_string();
    let out = out.to_path_buf();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([880.0, 960.0])
            .with_min_inner_size([420.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "mangadex-dl",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, &lang, &out, saver)))),
    )
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, lang: &str, out: &Path, saver: bool) -> Self {
        install_font(&cc.egui_ctx);
        cc.egui_ctx.set_theme(egui::ThemePreference::System);
        cc.egui_ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = egui::vec2(12.0, 8.0);
            style.spacing.button_padding = egui::vec2(10.0, 6.0);
            style.spacing.scroll.floating = false;
            style.spacing.scroll.bar_width = 8.0;
        });
        let (tx, rx, cancel) = spawn_worker(cc.egui_ctx.clone());
        let lang = crate::model::validate_lang(lang).unwrap_or_else(|_| "ru".to_string());
        let mut extra_langs = Vec::new();
        if !LANGS.iter().any(|(code, _)| *code == lang) {
            extra_langs.push(lang.clone());
        }
        let out = if out == Path::new("downloads") {
            default_out()
        } else {
            expand_path(&out.to_string_lossy()).display().to_string()
        };
        Self {
            tx,
            rx,
            cancel,
            generation: 0,
            query: String::new(),
            focus_query: true,
            lang,
            extra_langs,
            out,
            saver,
            spec: String::new(),
            notice: String::new(),
            results: Vec::new(),
            total: 0,
            manga: None,
            chapters: Vec::new(),
            selected: HashSet::new(),
            anchor: 0,
            reveal_title: false,
            busy: false,
            job: Job::default(),
            covers: HashMap::new(),
            theme_dark: None,
        }
    }

    fn pump(&mut self, ctx: &egui::Context) {
        loop {
            match self.rx.try_recv() {
                Ok(event) => self.apply(event, ctx),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
    }

    fn apply(&mut self, event: Event, ctx: &egui::Context) {
        match event {
            Event::Opened { gen, opened } => {
                if gen != self.generation {
                    return;
                }
                self.busy = false;
                self.lang = opened.lang;
                self.remember_languages(opened.manga.as_ref());
                for manga in &opened.results {
                    self.remember_languages(Some(manga));
                }
                self.results = opened.results;
                self.total = opened.total;
                self.manga = opened.manga;
                self.chapters = opened.chapters;
                self.selected = opened.select.into_iter().collect();
                self.anchor = 0;
                self.reveal_title = self.manga.is_some();
                self.notice.clear();
                if self.manga.is_some() && self.chapters.is_empty() {
                    self.notice = "На этом языке глав нет.".to_string();
                } else if self.manga.is_none() && self.results.is_empty() {
                    self.notice = "Ничего не нашлось.".to_string();
                }
                self.retain_covers();
            }
            Event::Failed { gen, message } => {
                if gen != self.generation || self.job.running {
                    return;
                }
                self.busy = false;
                self.notice = message;
            }
            Event::Cover { gen, id, bytes } => {
                if gen != self.generation || self.covers.contains_key(&id) {
                    return;
                }
                let known = self.results.iter().any(|manga| manga.id == id)
                    || self.manga.as_ref().is_some_and(|manga| manga.id == id);
                if !known {
                    return;
                }
                if let Some(texture) = texture_from_bytes(ctx, &id, &bytes) {
                    self.covers.insert(id, texture);
                }
            }
            Event::Progress {
                gen,
                done_chapters,
                chapter_count,
                page,
                pages,
                message,
            } => {
                if gen != self.generation {
                    return;
                }
                self.job.running = true;
                self.job.done_chapters = done_chapters;
                self.job.chapter_count = chapter_count;
                self.job.page = page;
                self.job.pages = pages.max(1);
                if !self.job.stopping {
                    self.job.message = message;
                }
            }
            Event::Finished {
                gen,
                message,
                stopped,
                failed,
                folder,
            } => {
                if gen != self.generation {
                    return;
                }
                self.busy = false;
                self.job.running = false;
                self.job.stopping = false;
                self.job.stopped = stopped;
                self.job.failed = failed;
                self.job.message = message;
                if folder.is_dir() {
                    self.job.folder = Some(folder);
                }
                if !stopped && !failed {
                    self.job.done_chapters = self.job.chapter_count;
                    self.job.page = 0;
                }
            }
        }
    }

    fn remember_languages(&mut self, manga: Option<&Manga>) {
        let Some(manga) = manga else { return };
        for code in &manga.languages {
            if LANGS.iter().any(|(id, _)| id == code)
                || self.extra_langs.iter().any(|item| item == code)
            {
                continue;
            }
            self.extra_langs.push(code.clone());
        }
    }

    fn retain_covers(&mut self) {
        let mut keep = HashSet::new();
        for manga in &self.results {
            keep.insert(manga.id.clone());
        }
        if let Some(manga) = &self.manga {
            keep.insert(manga.id.clone());
        }
        self.covers.retain(|id, _| keep.contains(id));
    }

    fn search(&mut self) {
        let query = self.query.trim().to_string();
        if query.is_empty() || self.job.running {
            return;
        }
        self.generation += 1;
        self.busy = true;
        self.notice = "Ищу…".to_string();
        let _ = self.tx.send(Command::Search {
            gen: self.generation,
            query,
            lang: self.lang.clone(),
        });
    }

    fn open_id(&mut self, id: &str) {
        if self.job.running {
            return;
        }
        self.generation += 1;
        self.busy = true;
        self.notice = "Собираю главы…".to_string();
        let _ = self.tx.send(Command::Open {
            gen: self.generation,
            id: id.to_string(),
            lang: self.lang.clone(),
        });
    }

    fn apply_spec(&mut self) {
        let spec = self.spec.trim().to_string();
        if spec.is_empty() {
            return;
        }
        match select_chapters(&self.chapters, &spec) {
            Ok(picked) => {
                self.selected = picked.into_iter().map(|chapter| chapter.id).collect();
                self.notice.clear();
                self.reveal_title = true;
            }
            Err(message) => self.notice = message,
        }
    }

    fn select_all(&mut self) {
        self.selected = self
            .chapters
            .iter()
            .filter(|chapter| chapter.external_url.is_none())
            .map(|chapter| chapter.id.clone())
            .collect();
    }

    fn download(&mut self) {
        let Some(manga) = self.manga.clone() else {
            return;
        };
        if self.job.running {
            return;
        }
        let chapters: Vec<Chapter> = self
            .chapters
            .iter()
            .filter(|chapter| self.selected.contains(&chapter.id))
            .cloned()
            .collect();
        if chapters.is_empty() {
            return;
        }
        let root = expand_path(&self.out);
        self.cancel.store(false, Ordering::Relaxed);
        self.generation += 1;
        self.busy = true;
        self.notice.clear();
        self.job = Job {
            running: true,
            message: "Начинаю".to_string(),
            chapter_count: chapters.len() as u32,
            pages: 1,
            ..Job::default()
        };
        let _ = self.tx.send(Command::Download {
            gen: self.generation,
            manga,
            lang: self.lang.clone(),
            chapters,
            root,
            saver: self.saver,
        });
    }

    fn stop(&mut self) {
        if !self.job.running {
            return;
        }
        self.cancel.store(true, Ordering::Relaxed);
        self.job.stopping = true;
        self.job.message = "Останавливаю…".to_string();
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let dark = ui.ctx().theme() == egui::Theme::Dark;
        if self.theme_dark != Some(dark) {
            let colors = palette(dark);
            ui.ctx().set_visuals(visuals(&colors, dark));
            self.theme_dark = Some(dark);
        }
        self.pump(ui.ctx());
        if self.busy || self.job.running {
            ui.ctx().request_repaint_after(Duration::from_millis(80));
        }
        let colors = palette(dark);
        // Нижняя панель вне прокрутки: она не должна закрывать строки глав.
        if self.manga.is_some() {
            let mut download = false;
            let mut stop = false;
            let mut reveal = false;
            egui::Panel::bottom("dock")
                .frame(dock_frame(&colors))
                .show(ui, |ui| {
                    centered(ui, |ui| {
                        (download, stop, reveal) = dock(self, ui, &colors);
                    });
                });
            if download {
                self.download();
            }
            if stop {
                self.stop();
            }
            if reveal {
                if let Some(folder) = &self.job.folder.clone() {
                    if let Err(message) = reveal_folder(folder) {
                        self.notice = message;
                    }
                }
            }
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(colors.bg)
                    .inner_margin(egui::Margin::symmetric(16, 28)),
            )
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        centered(ui, |ui| sheet(self, ui, &colors));
                    });
            });
    }
}

fn dock_frame(colors: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(colors.bg)
        .inner_margin(egui::Margin::symmetric(16, 12))
}

fn sheet(app: &mut App, ui: &mut egui::Ui, colors: &Palette) {
    ui.label(
        egui::RichText::new("MANGADEX-DL")
            .color(colors.accent)
            .size(12.0),
    );
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new("Личное чтение. Страницы выбранных глав сохраняются на диск.")
            .color(colors.muted),
    );
    ui.add_space(22.0);
    search_row(app, ui, colors);
    ui.add_space(8.0);
    if !app.notice.is_empty() {
        ui.label(egui::RichText::new(&app.notice).color(colors.accent));
    } else {
        ui.add_space(18.0);
    }
    results(app, ui, colors);
    if app.manga.is_some() {
        title_block(app, ui, colors);
        tools(app, ui, colors);
        chapters(app, ui, colors);
    }
    ui.add_space(28.0);
}

fn search_row(app: &mut App, ui: &mut egui::Ui, colors: &Palette) {
    let wide = ui.available_width() >= 560.0;
    let mut submit = false;
    if wide {
        ui.horizontal(|ui| {
            let query_width = (ui.available_width() - 250.0).max(140.0);
            ui.vertical(|ui| {
                ui.set_width(query_width);
                field_label(ui, "Тайтл или ссылка", colors);
                submit |= query_edit(app, ui, colors);
            });
            ui.vertical(|ui| {
                ui.set_width(156.0);
                field_label(ui, "Язык", colors);
                lang_box(app, ui);
            });
            ui.vertical(|ui| {
                ui.add_space(18.0);
                submit |= text_button(ui, "Найти", !app.job.running, colors).clicked();
            });
        });
    } else {
        field_label(ui, "Тайтл или ссылка", colors);
        submit |= query_edit(app, ui, colors);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_width((ui.available_width() - 80.0).max(100.0));
                field_label(ui, "Язык", colors);
                lang_box(app, ui);
            });
            ui.vertical(|ui| {
                ui.add_space(18.0);
                submit |= text_button(ui, "Найти", !app.job.running, colors).clicked();
            });
        });
    }
    if submit {
        app.search();
    }
}

fn query_edit(app: &mut App, ui: &mut egui::Ui, colors: &Palette) -> bool {
    let response = underline_edit(ui, &mut app.query, "название или адрес MangaDex", colors);
    if app.focus_query {
        response.request_focus();
        app.focus_query = false;
    }
    response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter))
}

fn lang_box(app: &mut App, ui: &mut egui::Ui) {
    let selected = lang_label(&app.lang);
    egui::ComboBox::from_id_salt("lang")
        .width(ui.available_width())
        .selected_text(selected)
        .show_ui(ui, |ui| {
            for (code, label) in LANGS {
                ui.selectable_value(&mut app.lang, (*code).to_string(), *label);
            }
            for code in &app.extra_langs {
                ui.selectable_value(&mut app.lang, code.clone(), code);
            }
        });
}

fn results(app: &mut App, ui: &mut egui::Ui, colors: &Palette) {
    if !show_result_list(app.manga.is_some(), app.results.len()) {
        return;
    }
    let shown = app.results.len();
    let note = if app.total > shown {
        format!(
            "Показаны {shown} из {}. Если нужного нет, вставьте ссылку на тайтл.",
            app.total
        )
    } else {
        format!("Найдено: {}.", app.total)
    };
    ui.label(egui::RichText::new(note).color(colors.muted));
    let mut open = None;
    for manga in &app.results {
        let current = app.manga.as_ref().is_some_and(|item| item.id == manga.id);
        let response = ui
            .horizontal(|ui| {
                thumb(
                    ui,
                    app.covers.get(&manga.id),
                    egui::vec2(36.0, 52.0),
                    colors,
                );
                ui.vertical(|ui| {
                    let title_color = if current { colors.accent } else { colors.ink };
                    ui.label(
                        egui::RichText::new(&manga.title)
                            .strong()
                            .color(title_color),
                    );
                    ui.label(
                        egui::RichText::new(manga_meta(manga))
                            .color(colors.muted)
                            .size(14.5),
                    );
                });
            })
            .response;
        let line = response.rect.x_range();
        ui.painter().hline(
            line,
            response.rect.top(),
            egui::Stroke::new(1.0, colors.line),
        );
        if response.interact(egui::Sense::click()).clicked() && !app.job.running {
            open = Some(manga.id.clone());
        }
    }
    if let Some(id) = open {
        app.open_id(&id);
    }
}

fn title_block(app: &mut App, ui: &mut egui::Ui, colors: &Palette) {
    let Some(manga) = app.manga.clone() else {
        return;
    };
    ui.add_space(18.0);
    let response = ui
        .horizontal(|ui| {
            thumb(
                ui,
                app.covers.get(&manga.id),
                egui::vec2(72.0, 104.0),
                colors,
            );
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new(&manga.title)
                        .strong()
                        .size(22.0)
                        .color(colors.ink),
                );
                ui.label(egui::RichText::new(manga_meta(&manga)).color(colors.muted));
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    for code in &manga.languages {
                        let pressed = code == &app.lang;
                        let text = if pressed {
                            egui::RichText::new(code).color(colors.ink)
                        } else {
                            egui::RichText::new(code).color(colors.muted)
                        };
                        let stroke = if pressed { colors.ink } else { colors.line };
                        let button = egui::Button::new(text)
                            .fill(colors.bg)
                            .stroke(egui::Stroke::new(1.0, stroke));
                        if ui.add_enabled(!app.job.running, button).clicked() {
                            app.lang = code.clone();
                            app.open_id(&manga.id);
                        }
                    }
                });
            });
        })
        .response;
    if app.reveal_title {
        response.scroll_to_me(None);
        app.reveal_title = false;
    }
}

fn spec_field(ui: &mut egui::Ui, spec: &mut String, colors: &Palette) -> bool {
    field_label(ui, "Отметить главы", colors);
    let response = underline_edit(ui, spec, "12, 1-5, all", colors);
    response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter))
}

fn tool_actions(ui: &mut egui::Ui, running: bool, colors: &Palette) -> (bool, bool, bool) {
    let apply = text_button(ui, "Отметить", !running, colors).clicked();
    let all = text_button(ui, "Все", !running, colors).clicked();
    let clear = text_button(ui, "Снять", !running, colors).clicked();
    (apply, all, clear)
}

fn tools(app: &mut App, ui: &mut egui::Ui, colors: &Palette) {
    ui.add_space(16.0);
    let running = app.job.running;
    let mut apply = false;
    let mut all = false;
    let mut clear = false;
    let wide = ui.available_width() >= 640.0;
    if wide {
        ui.horizontal(|ui| {
            let field_width = (ui.available_width() - 250.0).max(180.0);
            ui.vertical(|ui| {
                ui.set_width(field_width);
                apply |= spec_field(ui, &mut app.spec, colors);
            });
            ui.vertical(|ui| {
                ui.add_space(18.0);
                ui.horizontal(|ui| {
                    let (next_apply, next_all, next_clear) = tool_actions(ui, running, colors);
                    apply |= next_apply;
                    all |= next_all;
                    clear |= next_clear;
                });
            });
        });
    } else {
        apply |= spec_field(ui, &mut app.spec, colors);
        ui.horizontal(|ui| {
            let (next_apply, next_all, next_clear) = tool_actions(ui, running, colors);
            apply |= next_apply;
            all |= next_all;
            clear |= next_clear;
        });
    }
    if apply {
        app.apply_spec();
    }
    if all {
        app.select_all();
    }
    if clear {
        app.selected.clear();
    }
    let chosen: Vec<&Chapter> = app
        .chapters
        .iter()
        .filter(|chapter| app.selected.contains(&chapter.id))
        .collect();
    let count = if chosen.is_empty() {
        "Ничего не отмечено".to_string()
    } else {
        let pages: u32 = chosen.iter().map(|chapter| chapter.pages).sum();
        format!("Выбрано {} · {pages} стр.", chosen.len())
    };
    ui.label(egui::RichText::new(count).color(colors.muted));
}

fn chapters(app: &mut App, ui: &mut egui::Ui, colors: &Palette) {
    let narrow = ui.available_width() < 560.0;
    chapter_header(ui, narrow, colors);
    let count = app.chapters.len();
    for index in 0..count {
        let id = app.chapters[index].id.clone();
        let external = app.chapters[index].external_url.is_some();
        let number = app.chapters[index]
            .number
            .clone()
            .unwrap_or_else(|| "oneshot".to_string());
        let volume = app.chapters[index]
            .volume
            .as_ref()
            .filter(|value| !value.is_empty())
            .map(|value| format!("том {value}"))
            .unwrap_or_default();
        let pages = if external {
            "внешняя".to_string()
        } else {
            format!("{} стр.", app.chapters[index].pages)
        };
        let title = app.chapters[index].title.clone().unwrap_or_default();
        let group = credit(&app.chapters[index]);
        let name = if title.is_empty() {
            group
        } else {
            format!("{group} · {title}")
        };
        let mut on = app.selected.contains(&id);
        let mut toggled = false;
        let response = ui
            .horizontal(|ui| {
                toggled = ui
                    .add_enabled(
                        !external && !app.job.running,
                        egui::Checkbox::new(&mut on, ""),
                    )
                    .changed();
                cell(ui, &number, 92.0, colors.ink);
                if !narrow {
                    cell(ui, &volume, 78.0, colors.muted);
                }
                cell(ui, &pages, 78.0, colors.ink);
                ui.add(
                    egui::Label::new(egui::RichText::new(name).color(colors.muted))
                        .truncate()
                        .wrap_mode(egui::TextWrapMode::Truncate),
                );
            })
            .response;
        ui.painter().hline(
            response.rect.x_range(),
            response.rect.bottom(),
            egui::Stroke::new(1.0, colors.line),
        );
        if toggled {
            let shift = ui.input(|input| input.modifiers.shift);
            let ids: Vec<String> = if shift {
                let from = app.anchor.min(index);
                let to = app.anchor.max(index);
                app.chapters[from..=to]
                    .iter()
                    .filter(|chapter| chapter.external_url.is_none())
                    .map(|chapter| chapter.id.clone())
                    .collect()
            } else if !external {
                vec![id]
            } else {
                Vec::new()
            };
            for item in ids {
                if on {
                    app.selected.insert(item);
                } else {
                    app.selected.remove(&item);
                }
            }
            app.anchor = index;
        }
    }
}

fn chapter_header(ui: &mut egui::Ui, narrow: bool, colors: &Palette) {
    let response = ui
        .horizontal(|ui| {
            ui.add_space(22.0);
            cell(ui, "глава", 92.0, colors.muted);
            if !narrow {
                cell(ui, "том", 78.0, colors.muted);
            }
            cell(ui, "стр.", 78.0, colors.muted);
            ui.label(
                egui::RichText::new("перевод")
                    .size(12.0)
                    .color(colors.muted),
            );
        })
        .response;
    ui.painter().hline(
        response.rect.x_range(),
        response.rect.bottom(),
        egui::Stroke::new(1.0, colors.line),
    );
}

fn dock(app: &mut App, ui: &mut egui::Ui, colors: &Palette) -> (bool, bool, bool) {
    let running = app.job.running;
    let chosen = app
        .chapters
        .iter()
        .any(|chapter| app.selected.contains(&chapter.id));
    let mut download = false;
    let mut stop = false;
    let wide = ui.available_width() >= 560.0;
    if wide {
        ui.horizontal(|ui| {
            let folder_width = (ui.available_width() - 220.0).max(160.0);
            ui.vertical(|ui| {
                ui.set_width(folder_width);
                field_label(ui, "Папка", colors);
                underline_edit(ui, &mut app.out, "", colors);
            });
            ui.vertical(|ui| {
                ui.add_space(18.0);
                ui.add_enabled(!running, egui::Checkbox::new(&mut app.saver, "сжатые"));
            });
            ui.vertical(|ui| {
                ui.add_space(14.0);
                if running {
                    stop = primary(ui, "Остановить", true, colors);
                } else {
                    download = primary(ui, "Скачать", chosen, colors);
                }
            });
        });
    } else {
        field_label(ui, "Папка", colors);
        underline_edit(ui, &mut app.out, "", colors);
        ui.add_enabled(!running, egui::Checkbox::new(&mut app.saver, "сжатые"));
        if running {
            stop = primary(ui, "Остановить", true, colors);
        } else {
            download = primary(ui, "Скачать", chosen, colors);
        }
    }
    let mut reveal = false;
    if running || !app.job.message.is_empty() {
        ui.add_space(8.0);
        let fraction = progress_fraction(
            app.job.done_chapters,
            app.job.page,
            app.job.pages,
            app.job.chapter_count,
        );
        bar(ui, fraction, colors);
        let color = if app.job.failed || app.job.stopping || app.job.stopped {
            colors.accent
        } else if !running && app.job.message.starts_with("Готово") {
            colors.ok
        } else {
            colors.ink
        };
        ui.label(egui::RichText::new(&app.job.message).color(color));
        if !running
            && app.job.folder.as_ref().is_some_and(|path| path.is_dir())
            && text_button(ui, "Открыть папку", true, colors).clicked()
        {
            reveal = true;
        }
    }
    (download, stop, reveal)
}

fn bar(ui: &mut egui::Ui, fraction: f32, colors: &Palette) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 4.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, colors.track);
    let mut fill = rect;
    fill.set_width(rect.width() * fraction.clamp(0.0, 1.0));
    ui.painter().rect_filled(fill, 0.0, colors.accent);
}

fn primary(ui: &mut egui::Ui, label: &str, enabled: bool, colors: &Palette) -> bool {
    let text = egui::RichText::new(label).color(if enabled {
        colors.accent_ink
    } else {
        colors.muted
    });
    let fill = if enabled { colors.accent } else { colors.paper };
    let button = egui::Button::new(text)
        .fill(fill)
        .stroke(egui::Stroke::NONE)
        .min_size(egui::vec2(108.0, 36.0));
    let response = ui.add(button);
    enabled && response.clicked()
}

fn text_button(ui: &mut egui::Ui, label: &str, enabled: bool, colors: &Palette) -> egui::Response {
    let text = egui::RichText::new(label).color(if enabled { colors.ink } else { colors.muted });
    let response = ui.add_enabled(enabled, egui::Button::new(text).frame(false));
    let line = if enabled { colors.ink } else { colors.line };
    ui.painter().hline(
        response.rect.x_range(),
        response.rect.bottom(),
        egui::Stroke::new(1.0, line),
    );
    response
}

fn field_label(ui: &mut egui::Ui, text: &str, colors: &Palette) {
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .size(11.0)
            .color(colors.muted),
    );
}

fn underline_edit(
    ui: &mut egui::Ui,
    text: &mut String,
    hint: &str,
    colors: &Palette,
) -> egui::Response {
    let response = ui.add(
        egui::TextEdit::singleline(text)
            .desired_width(f32::INFINITY)
            .frame(egui::Frame::NONE)
            .hint_text(hint),
    );
    let color = if response.has_focus() {
        colors.accent
    } else {
        colors.line
    };
    ui.painter().hline(
        response.rect.x_range(),
        response.rect.bottom(),
        egui::Stroke::new(1.0, color),
    );
    response
}

fn cell(ui: &mut egui::Ui, text: &str, width: f32, color: egui::Color32) {
    ui.allocate_ui_with_layout(
        [width, 18.0].into(),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_width(width);
            ui.label(egui::RichText::new(text).color(color));
        },
    );
}

fn thumb(
    ui: &mut egui::Ui,
    texture: Option<&egui::TextureHandle>,
    size: egui::Vec2,
    colors: &Palette,
) {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(1.0, colors.line),
        egui::StrokeKind::Inside,
    );
    if let Some(texture) = texture {
        ui.painter().image(
            texture.id(),
            rect,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    } else {
        ui.painter()
            .rect_filled(rect.shrink(1.0), 0.0, colors.paper);
    }
}

fn centered(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let available = ui.available_width();
    let width = available.min(760.0);
    let side = ((available - width) * 0.5).max(0.0);
    ui.horizontal(|ui| {
        if side > 0.0 {
            ui.add_space(side);
        }
        ui.vertical(|ui| {
            ui.set_width(width);
            add(ui);
        });
    });
}

fn manga_meta(manga: &Manga) -> String {
    let year = manga
        .year
        .map(|value| value.to_string())
        .unwrap_or_else(|| "—".to_string());
    let status = status_label(manga.status.as_deref());
    let mut bits = vec![year, status.to_string()];
    if !manga.original_title.is_empty()
        && manga.original_title.to_lowercase() != manga.title.to_lowercase()
    {
        bits.push(manga.original_title.clone());
    }
    bits.join(" · ")
}

fn lang_label(code: &str) -> String {
    LANGS
        .iter()
        .find(|(id, _)| *id == code)
        .map(|(_, label)| (*label).to_string())
        .unwrap_or_else(|| code.to_string())
}

fn show_result_list(has_manga: bool, count: usize) -> bool {
    count > 0 && !(has_manga && count == 1)
}

fn progress_fraction(done_chapters: u32, page: u32, pages: u32, chapter_count: u32) -> f32 {
    if chapter_count == 0 {
        return 0.0;
    }
    let pages = pages.max(1) as f32;
    ((done_chapters as f32) + (page as f32 / pages)) / chapter_count as f32
}

fn default_out() -> String {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("downloads")
        .display()
        .to_string()
}

fn expand_path(text: &str) -> PathBuf {
    let trimmed = text.trim();
    if trimmed == "~" {
        return home().unwrap_or_else(|| PathBuf::from(trimmed));
    }
    if let Some(rest) = trimmed
        .strip_prefix("~/")
        .or_else(|| trimmed.strip_prefix("~\\"))
    {
        if let Some(home) = home() {
            return home.join(rest);
        }
    }
    PathBuf::from(trimmed)
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn reveal_folder(path: &Path) -> Result<(), String> {
    if !path.is_dir() {
        return Err("папка ещё не создана".to_string());
    }
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn install_font(ctx: &egui::Context) {
    let Some(bytes) = system_font() else { return };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "ui".to_string(),
        Arc::new(egui::FontData::from_owned(bytes)),
    );
    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        family.insert(0, "ui".to_string());
    }
    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
        family.insert(0, "ui".to_string());
    }
    ctx.set_fonts(fonts);
}

fn system_font() -> Option<Vec<u8>> {
    const CANDIDATES: &[&str] = &[
        "/usr/share/fonts/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        r"C:\Windows\Fonts\segoeui.ttf",
        r"C:\Windows\Fonts\arial.ttf",
    ];
    CANDIDATES.iter().find_map(|path| std::fs::read(path).ok())
}

fn texture_from_bytes(ctx: &egui::Context, id: &str, bytes: &[u8]) -> Option<egui::TextureHandle> {
    let image = image::load_from_memory(bytes).ok()?;
    let rgba = image.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
    Some(ctx.load_texture(format!("cover-{id}"), color, egui::TextureOptions::LINEAR))
}

fn spawn_worker(ctx: egui::Context) -> (Sender<Command>, Receiver<Event>, Arc<AtomicBool>) {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (evt_tx, evt_rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    thread::Builder::new()
        .name("mangadex".to_string())
        .spawn(move || worker(cmd_rx, evt_tx, ctx, worker_cancel))
        .expect("поток загрузки");
    (cmd_tx, evt_rx, cancel)
}

fn worker(rx: Receiver<Command>, tx: Sender<Event>, ctx: egui::Context, cancel: Arc<AtomicBool>) {
    let client = match Client::new() {
        Ok(client) => client,
        Err(error) => {
            emit(
                &tx,
                &ctx,
                Event::Failed {
                    gen: 0,
                    message: error.message,
                },
            );
            return;
        }
    };
    let mut pending = None;
    loop {
        let command = if let Some(command) = pending.take() {
            command
        } else {
            match rx.recv() {
                Ok(command) => command,
                Err(_) => return,
            }
        };
        pending = dispatch(&client, &rx, &tx, &ctx, &cancel, command);
    }
}

fn dispatch(
    client: &Client,
    rx: &Receiver<Command>,
    tx: &Sender<Event>,
    ctx: &egui::Context,
    cancel: &AtomicBool,
    command: Command,
) -> Option<Command> {
    match command {
        Command::Search { gen, query, lang } => match session::open_query(client, &query, &lang) {
            Ok(opened) => {
                let jobs = cover_jobs(&opened);
                emit(tx, ctx, Event::Opened { gen, opened });
                fetch_covers(client, rx, tx, ctx, gen, jobs)
            }
            Err(error) => {
                emit(
                    tx,
                    ctx,
                    Event::Failed {
                        gen,
                        message: error.message,
                    },
                );
                None
            }
        },
        Command::Open { gen, id, lang } => match session::open_manga(client, &id, &lang) {
            Ok(opened) => {
                let jobs = cover_jobs(&opened);
                emit(tx, ctx, Event::Opened { gen, opened });
                fetch_covers(client, rx, tx, ctx, gen, jobs)
            }
            Err(error) => {
                emit(
                    tx,
                    ctx,
                    Event::Failed {
                        gen,
                        message: error.message,
                    },
                );
                None
            }
        },
        Command::Download {
            gen,
            manga,
            lang,
            chapters,
            root,
            saver,
        } => {
            run_download(
                client, tx, ctx, cancel, gen, &manga, &lang, &chapters, &root, saver,
            );
            None
        }
    }
}

fn cover_jobs(opened: &Opened) -> Vec<(String, String)> {
    let mut jobs = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |manga: &Manga| {
        if let Some(url) = &manga.cover {
            if seen.insert(manga.id.clone()) {
                jobs.push((manga.id.clone(), url.clone()));
            }
        }
    };
    if let Some(manga) = &opened.manga {
        push(manga);
    }
    for manga in &opened.results {
        push(manga);
    }
    jobs
}

fn fetch_covers(
    client: &Client,
    rx: &Receiver<Command>,
    tx: &Sender<Event>,
    ctx: &egui::Context,
    gen: u64,
    jobs: Vec<(String, String)>,
) -> Option<Command> {
    let idle = AtomicBool::new(false);
    for (id, url) in jobs {
        match rx.try_recv() {
            Ok(command) => return Some(command),
            Err(TryRecvError::Disconnected) => return None,
            Err(TryRecvError::Empty) => {}
        }
        if let Ok((true, bytes, _, _)) = client.fetch_image(&url, &idle) {
            emit(tx, ctx, Event::Cover { gen, id, bytes });
        }
    }
    None
}

fn run_download(
    client: &Client,
    tx: &Sender<Event>,
    ctx: &egui::Context,
    cancel: &AtomicBool,
    gen: u64,
    manga: &Manga,
    lang: &str,
    chapters: &[Chapter],
    root: &Path,
    saver: bool,
) {
    let chapter_count = chapters.len() as u32;
    let mut downloaded = 0u32;
    let mut skipped = 0u32;
    let mut failed = 0u32;
    let mut folder = root.to_path_buf();
    let mut stopped = false;
    let mut failure = None;
    for (index, chapter) in chapters.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            stopped = true;
            break;
        }
        let label = chapter
            .number
            .clone()
            .unwrap_or_else(|| "oneshot".to_string());
        emit(
            tx,
            ctx,
            Event::Progress {
                gen,
                done_chapters: index as u32,
                chapter_count,
                page: 0,
                pages: chapter.pages.max(1),
                message: format!("Глава {label}"),
            },
        );
        let tx_page = tx.clone();
        let ctx_page = ctx.clone();
        let page_label = label.clone();
        let mut callback = move |page: u32, pages: u32, state: &str| {
            let message = if state == "failed" {
                format!("Глава {page_label}, страница {page}: не скачалась")
            } else {
                format!("Глава {page_label} · {page} из {pages}")
            };
            emit(
                &tx_page,
                &ctx_page,
                Event::Progress {
                    gen,
                    done_chapters: index as u32,
                    chapter_count,
                    page,
                    pages,
                    message,
                },
            );
        };
        match download_chapter(
            client,
            manga,
            lang,
            chapter,
            root,
            saver,
            false,
            cancel,
            Some(&mut callback),
        ) {
            Ok(result) => {
                downloaded += result.downloaded;
                skipped += result.skipped;
                failed += result.failed;
                folder = chapter_dir(root, manga, lang, chapter);
            }
            Err(Fail::Cancelled) => {
                stopped = true;
                break;
            }
            Err(error) => {
                failure = Some(error.to_string());
                break;
            }
        }
    }
    let (message, failed_job) = if stopped {
        ("Остановлено".to_string(), false)
    } else if let Some(message) = failure {
        (message, true)
    } else {
        let mut message = format!("Готово. Новых {downloaded}, уже было {skipped}.");
        if failed > 0 {
            message.push_str(&format!(" Ошибок: {failed}."));
        }
        (message, false)
    };
    emit(
        tx,
        ctx,
        Event::Finished {
            gen,
            message,
            stopped,
            failed: failed_job,
            folder,
        },
    );
}

fn emit(tx: &Sender<Event>, ctx: &egui::Context, event: Event) {
    let _ = tx.send(event);
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_open_hides_the_result_list() {
        assert!(!show_result_list(true, 1));
        assert!(show_result_list(true, 4));
        assert!(!show_result_list(false, 0));
    }

    #[test]
    fn progress_fills_when_the_last_chapter_finishes() {
        assert!((progress_fraction(1, 5, 10, 2) - 0.75).abs() < 0.001);
        assert!((progress_fraction(2, 0, 1, 2) - 1.0).abs() < 0.001);
        assert_eq!(progress_fraction(0, 0, 1, 0), 0.0);
    }

    #[test]
    fn relative_folder_stays_relative() {
        assert_eq!(expand_path("downloads"), PathBuf::from("downloads"));
        assert_eq!(expand_path("  downloads  "), PathBuf::from("downloads"));
    }
}
