"""Локальный интерфейс загрузчика. Слушает только 127.0.0.1."""

from __future__ import annotations

import json
import subprocess
import sys
import threading
import traceback
import webbrowser
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import urlparse

from mangadex_dl import (
    ApiError,
    Cancelled,
    Chapter,
    Client,
    Manga,
    chapter_dir,
    classify_target,
    credit,
    download_chapter,
    select_chapters,
    validate_lang,
)


def ui_file() -> Path:
    root = Path(getattr(sys, "_MEIPASS", Path(__file__).resolve().parent))
    return root / "ui.html"


def public_manga(manga: Manga) -> dict[str, Any]:
    return {
        "id": manga.id,
        "title": manga.title,
        "original_title": manga.original_title,
        "year": manga.year,
        "status": manga.status,
        "original_language": manga.original_language,
        "languages": list(manga.languages),
        "cover": manga.cover,
    }


def public_chapter(chapter: Chapter, index: int) -> dict[str, Any]:
    return {
        "id": chapter.id,
        "index": index,
        "number": chapter.number or "oneshot",
        "volume": chapter.volume or "",
        "title": chapter.title or "",
        "pages": chapter.pages,
        "group": credit(chapter),
        "external": bool(chapter.external_url),
    }


class Job:
    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._cancel = threading.Event()
        self._state: dict[str, Any] = self._empty()

    @staticmethod
    def _empty() -> dict[str, Any]:
        return {
            "running": False,
            "message": "",
            "chapter": "",
            "page": 0,
            "pages": 0,
            "done_chapters": 0,
            "total_chapters": 0,
            "downloaded": 0,
            "skipped": 0,
            "failed": 0,
            "folder": "",
            "error": "",
            "stopped": False,
            "hold": 0.0,
        }

    def cancelled(self) -> bool:
        return self._cancel.is_set()

    def try_begin(self, total: int) -> bool:
        with self._lock:
            if self._state["running"]:
                return False
            self._cancel.clear()
            self._state = self._empty()
            self._state["running"] = True
            self._state["total_chapters"] = total
            self._state["message"] = "Начинаю"
            return True

    def update(self, **fields: Any) -> None:
        with self._lock:
            self._state.update(fields)
            self._state["hold"] = self._fraction_locked()

    def finish(self, **fields: Any) -> None:
        with self._lock:
            self._state.update(fields)
            self._state["running"] = False
            self._state["hold"] = self._fraction_locked()

    def _fraction_locked(self) -> float:
        total = self._state["total_chapters"] or 1
        pages = self._state["pages"] or 1
        if str(self._state["message"]).startswith("Готово"):
            return 1.0
        if self._state["running"]:
            return max(0.0, min(1.0, (self._state["done_chapters"] + (self._state["page"] / pages)) / total))
        return float(self._state.get("hold") or 0.0)

    def view(self) -> dict[str, Any]:
        with self._lock:
            data = dict(self._state)
            data["progress"] = self._fraction_locked()
        data.pop("hold", None)
        return data


class App:
    def __init__(self, client: Any = None, out: Path | None = None) -> None:
        self.client = client or Client()
        self.out = str((out or (Path.cwd() / "downloads")).expanduser())
        self.lang = "ru"
        self.results: list[Manga] = []
        self.total = 0
        self.manga: Manga | None = None
        self.chapters: list[Chapter] = []
        self.job = Job()
        self._lock = threading.Lock()

    def snapshot(self, select: list[str] | None = None) -> dict[str, Any]:
        with self._lock:
            manga = self.manga
            chapters = list(self.chapters)
            results = list(self.results)
            total = self.total
            lang = self.lang
            out = self.out
        return {
            "lang": lang,
            "out": out,
            "results": [public_manga(item) for item in results],
            "total": total,
            "manga": public_manga(manga) if manga else None,
            "chapters": [public_chapter(chapter, index) for index, chapter in enumerate(chapters, 1)],
            "select": select or [],
            "job": self.job.view(),
        }

    def search(self, query: str, lang: str) -> dict[str, Any]:
        cleaned = query.strip()
        if not cleaned:
            raise ValueError("пустой запрос")
        self.lang = validate_lang(lang)
        kind, value = classify_target(cleaned)
        if kind == "chapter":
            return self._open_chapter(value)
        if kind in {"manga", "uuid"}:
            try:
                return self._open_id(value)
            except ApiError as exc:
                if kind != "uuid" or exc.status != 404:
                    raise
                return self._open_chapter(value)
        found, total = self.client.search(cleaned, self.lang)
        with self._lock:
            self.results = found
            self.total = total
        exact = [
            manga
            for manga in found
            if any(alias.casefold() == cleaned.casefold() for alias in manga.aliases)
        ]
        if len(exact) == 1:
            return self._use_manga(exact[0])
        if len(found) == 1:
            return self._use_manga(found[0])
        with self._lock:
            self.manga = None
            self.chapters = []
        return self.snapshot()

    def open_manga(self, manga_id: str, lang: str) -> dict[str, Any]:
        self.lang = validate_lang(lang)
        return self._open_id(manga_id.strip())

    def _open_id(self, manga_id: str) -> dict[str, Any]:
        return self._use_manga(self.client.get_manga(manga_id, self.lang), replace_results=True)

    def _open_chapter(self, chapter_id: str) -> dict[str, Any]:
        manga, chapter = self.client.chapter_with_manga(chapter_id, self.lang)
        if chapter.language:
            self.lang = validate_lang(chapter.language)
            manga = self.client.get_manga(manga.id, self.lang)
        return self._use_manga(manga, select=[chapter.id], replace_results=True)

    def _use_manga(
        self,
        manga: Manga,
        select: list[str] | None = None,
        *,
        replace_results: bool = False,
    ) -> dict[str, Any]:
        chapters = self.client.chapters(manga.id, self.lang)
        with self._lock:
            self.manga = manga
            self.chapters = chapters
            if replace_results:
                self.results = [manga]
                self.total = 1
        return self.snapshot(select)

    def match(self, spec: str) -> dict[str, Any]:
        with self._lock:
            chapters = list(self.chapters)
        picked = select_chapters(chapters, spec)
        return {"ids": [chapter.id for chapter in picked]}

    def start_download(self, ids: list[str], out: str, saver: bool) -> dict[str, Any]:
        if self.job.view()["running"]:
            raise ValueError("загрузка уже идёт")
        wanted = {item for item in ids if isinstance(item, str)}
        with self._lock:
            manga = self.manga
            chapters = [chapter for chapter in self.chapters if chapter.id in wanted]
            lang = self.lang
        if manga is None or not chapters:
            raise ValueError("главы не выбраны")
        folder = Path(out or self.out).expanduser()
        self.out = str(folder)
        if not self.job.try_begin(len(chapters)):
            raise ValueError("загрузка уже идёт")

        def work() -> None:
            downloaded = skipped = failed = 0
            last_folder = folder
            try:
                for index, chapter in enumerate(chapters):
                    if self.job.cancelled():
                        raise Cancelled()
                    label = chapter.number or "oneshot"
                    self.job.update(
                        chapter=label,
                        done_chapters=index,
                        page=0,
                        pages=max(chapter.pages, 1),
                        message=f"Глава {label}",
                    )

                    def on_page(page: int, pages: int, _state: str, current: str = label) -> None:
                        self.job.update(page=page, pages=pages, message=f"Глава {current} · {page} из {pages}")

                    result = download_chapter(
                        self.client,
                        manga,
                        lang,
                        chapter,
                        folder,
                        saver=saver,
                        force=False,
                        on_page=on_page,
                        should_stop=self.job.cancelled,
                    )
                    downloaded += result.downloaded
                    skipped += result.skipped
                    failed += result.failed
                    last_folder = chapter_dir(folder, manga, lang, chapter)
                    self.job.update(done_chapters=index + 1, folder=str(last_folder))
                message = f"Готово. Новых {downloaded}, уже было {skipped}."
                if failed:
                    message += f" Ошибок: {failed}."
                self.job.finish(
                    message=message,
                    downloaded=downloaded,
                    skipped=skipped,
                    failed=failed,
                    folder=str(last_folder),
                    done_chapters=len(chapters),
                    page=0,
                )
            except Cancelled:
                self.job.finish(message="Остановлено", stopped=True, folder=str(last_folder))
            except Exception as exc:
                self.job.finish(message="", error=str(exc), folder=str(last_folder))

        threading.Thread(target=work, daemon=True).start()
        return self.job.view()

    def cancel(self) -> dict[str, Any]:
        self.job._cancel.set()
        with self.job._lock:
            if self.job._state["running"]:
                self.job._state["message"] = "Останавливаю…"
        return self.job.view()

    def reveal(self) -> dict[str, Any]:
        folder = Path(self.job.view()["folder"])
        if not folder.is_dir():
            raise ValueError("папка ещё не создана")
        if sys.platform == "win32":
            import os

            os.startfile(folder)  # type: ignore[attr-defined]
        elif sys.platform == "darwin":
            subprocess.run(["open", str(folder)], check=False)
        else:
            subprocess.run(["xdg-open", str(folder)], check=False)
        return {"ok": True}


def read_json(handler: BaseHTTPRequestHandler) -> dict[str, Any]:
    length = int(handler.headers.get("Content-Length") or 0)
    if length > 1_000_000:
        raise ValueError("слишком большой запрос")
    raw = handler.rfile.read(length) if length else b"{}"
    payload = json.loads(raw.decode("utf-8") or "{}")
    if not isinstance(payload, dict):
        raise ValueError("ожидался объект")
    return payload


def send_json(handler: BaseHTTPRequestHandler, payload: dict[str, Any], status: int = 200) -> None:
    body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    handler.send_response(status)
    handler.send_header("Content-Type", "application/json; charset=utf-8")
    handler.send_header("Content-Length", str(len(body)))
    handler.send_header("Cache-Control", "no-store")
    handler.end_headers()
    handler.wfile.write(body)


def send_html(handler: BaseHTTPRequestHandler) -> None:
    body = ui_file().read_bytes()
    handler.send_response(200)
    handler.send_header("Content-Type", "text/html; charset=utf-8")
    handler.send_header("Content-Length", str(len(body)))
    handler.send_header("Cache-Control", "no-store")
    handler.end_headers()
    handler.wfile.write(body)


def make_handler(app: App) -> type[BaseHTTPRequestHandler]:
    class Handler(BaseHTTPRequestHandler):
        def do_GET(self) -> None:
            self._dispatch("GET")

        def do_POST(self) -> None:
            self._dispatch("POST")

        def _dispatch(self, method: str) -> None:
            path = urlparse(self.path).path
            try:
                if method == "GET" and path == "/":
                    send_html(self)
                    return
                if method == "GET" and path == "/api/state":
                    send_json(self, app.snapshot())
                    return
                if method == "GET" and path == "/api/job":
                    send_json(self, app.job.view())
                    return
                payload = read_json(self) if method == "POST" else {}
                if path == "/api/search":
                    send_json(self, app.search(str(payload.get("query") or ""), str(payload.get("lang") or "ru")))
                    return
                if path == "/api/open":
                    send_json(self, app.open_manga(str(payload.get("id") or ""), str(payload.get("lang") or app.lang)))
                    return
                if path == "/api/match":
                    send_json(self, app.match(str(payload.get("spec") or "")))
                    return
                if path == "/api/download":
                    ids = payload.get("ids") or []
                    if not isinstance(ids, list):
                        raise ValueError("нужен список глав")
                    send_json(self, app.start_download(ids, str(payload.get("out") or ""), bool(payload.get("saver"))))
                    return
                if path == "/api/cancel":
                    send_json(self, app.cancel())
                    return
                if path == "/api/reveal":
                    send_json(self, app.reveal())
                    return
                send_json(self, {"error": "не найдено"}, 404)
            except json.JSONDecodeError:
                send_json(self, {"error": "непонятный запрос"}, 400)
            except (ApiError, ValueError) as exc:
                send_json(self, {"error": str(exc)}, 400)
            except Exception as exc:
                traceback.print_exc()
                send_json(self, {"error": str(exc)}, 500)

        def log_message(self, format: str, *args: Any) -> None:
            return

    return Handler


class Server(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True


def serve(open_browser: bool = True) -> int:
    app = App()
    server = Server(("127.0.0.1", 0), make_handler(app))
    host, port = server.server_address[:2]
    url = f"http://{host}:{port}/"
    print(f"Интерфейс: {url}")
    print("Закройте это окно или нажмите Ctrl+C, чтобы остановить программу.")
    if open_browser:
        try:
            webbrowser.open(url)
        except Exception:
            pass
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nОстановлено")
        return 130
    finally:
        server.server_close()
    return 0
