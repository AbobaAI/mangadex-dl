#!/usr/bin/env python3
"""Личный некоммерческий загрузчик страниц глав с mangadex.org.

Один скрипт для Windows и Linux. Нужен только Python 3.10+.
Ходит в официальный API MangaDex, соблюдает паузу между запросами
и сообщает MangaDex@Home об успехе или ошибке загрузки страницы.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import time
import unicodedata
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.parse import quote, urlencode, urlparse
from urllib.request import Request, urlopen

API = "https://api.mangadex.org"
REPORT_URL = "https://api.mangadex.network/report"
USER_AGENT = (
    "mangadex-dl/1.0.0 (personal non-commercial client; "
    "+https://github.com/AbobaAI/mangadex-dl)"
)
CONTENT_RATINGS = ("safe", "suggestive", "erotica", "pornographic")
UUID_RE = re.compile(
    r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}"
)
_ILLEGAL_FILENAME = re.compile(r'[<>:"/\\|?*\x00-\x1f]')
_RESERVED_WINDOWS = {
    "CON",
    "PRN",
    "AUX",
    "NUL",
    *(f"COM{i}" for i in range(1, 10)),
    *(f"LPT{i}" for i in range(1, 10)),
}
_LANG_RE = re.compile(r"^[a-z]{2,3}(-[a-z]{2,8})?$")


class ApiError(Exception):
    def __init__(self, message: str, status: int | None = None) -> None:
        super().__init__(message)
        self.status = status


@dataclass
class Manga:
    id: str
    title: str
    original_title: str
    year: int | None
    status: str | None
    original_language: str | None
    languages: tuple[str, ...]
    aliases: tuple[str, ...]


@dataclass
class Chapter:
    id: str
    number: str | None
    volume: str | None
    title: str | None
    language: str
    pages: int
    group: str
    updated_at: str
    external_url: str | None


@dataclass
class DownloadResult:
    downloaded: int = 0
    skipped: int = 0
    failed: int = 0


class Limiter:
    def __init__(self, interval: float) -> None:
        self.interval = interval
        self._next = 0.0

    def wait(self) -> None:
        now = time.monotonic()
        if now < self._next:
            time.sleep(self._next - now)
        self._next = time.monotonic() + self.interval


def configure_stdio() -> None:
    for stream in (sys.stdin, sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is None:
            continue
        kwargs: dict[str, Any] = {"encoding": "utf-8", "errors": "replace"}
        if stream is not sys.stdin:
            kwargs["line_buffering"] = True
        try:
            reconfigure(**kwargs)
        except (OSError, ValueError, TypeError):
            try:
                reconfigure(encoding="utf-8", errors="replace")
            except (OSError, ValueError):
                pass


def one_line(text: str) -> str:
    return re.sub(r"\s+", " ", text).strip()


def sanitize(name: str, limit: int = 60) -> str:
    cleaned = unicodedata.normalize("NFC", name)
    cleaned = _ILLEGAL_FILENAME.sub(" ", cleaned)
    cleaned = re.sub(r"\s+", " ", cleaned).strip(" .")
    if not cleaned:
        cleaned = "untitled"
    if cleaned.split(".", 1)[0].upper() in _RESERVED_WINDOWS:
        cleaned = f"_{cleaned}"
    if len(cleaned) > limit:
        cleaned = cleaned[:limit].rstrip(" .")
    return cleaned or "untitled"


def decimal_places(value: str) -> int:
    if "." not in value:
        return 0
    return len(value.split(".", 1)[1].rstrip("0"))


def same_number(spec: str, value: str | None) -> bool:
    if value is None or not str(value).strip():
        return spec.casefold() in {"oneshot", "ваншот"}
    left = spec.strip()
    right = str(value).strip()
    if left == right:
        return True
    try:
        if float(left) != float(right):
            return False
    except ValueError:
        return False
    return decimal_places(left) == decimal_places(right)


def num_key(value: str | None) -> tuple[int, float, str]:
    if value is None or not str(value).strip():
        return (0, float("-inf"), "")
    text = str(value).strip()
    try:
        return (0, float(text), text)
    except ValueError:
        return (1, 0.0, text)


def sort_chapters(chapters: list[Chapter]) -> list[Chapter]:
    return sorted(
        chapters,
        key=lambda chapter: (
            num_key(chapter.volume),
            num_key(chapter.number),
            chapter.updated_at or "",
            chapter.id,
        ),
    )


def latest_per_slot(chapters: list[Chapter]) -> list[Chapter]:
    best: dict[tuple[str, str], Chapter] = {}
    order: list[tuple[str, str]] = []
    for chapter in chapters:
        key = (chapter.volume or "", chapter.number or "")
        current = best.get(key)
        if current is None:
            best[key] = chapter
            order.append(key)
            continue
        if (chapter.updated_at or "") >= (current.updated_at or ""):
            best[key] = chapter
    return [best[key] for key in order]


def split_range(part: str) -> tuple[str, str] | None:
    if part.count("-") != 1:
        return None
    left, right = (piece.strip() for piece in part.split("-", 1))
    if not left or not right:
        return None
    try:
        left_value = float(left)
        right_value = float(right)
    except ValueError:
        return None
    if left_value > right_value:
        raise ValueError(f"диапазон {part} перевёрнут")
    return left, right


def in_numeric_range(number: str, left: str, right: str) -> bool:
    try:
        value = float(number)
    except ValueError:
        return False
    return float(left) <= value <= float(right)


def chapter_by_index(chapters: list[Chapter], part: str) -> Chapter:
    raw = part[1:].strip()
    if not raw.isdigit():
        raise ValueError(f"неверный номер строки {part}")
    index = int(raw)
    if index < 1 or index > len(chapters):
        raise ValueError(f"нет строки {part}, в списке {len(chapters)}")
    return chapters[index - 1]


def select_chapters(chapters: list[Chapter], spec: str) -> list[Chapter]:
    text = spec.strip()
    if not text:
        raise ValueError("пустой номер главы")
    if text.casefold() in {"all", "*", "все"}:
        return latest_per_slot(chapters)

    picked: list[Chapter] = []
    seen: set[str] = set()

    def add(items: list[Chapter]) -> None:
        for chapter in items:
            if chapter.id in seen:
                continue
            picked.append(chapter)
            seen.add(chapter.id)

    for raw_part in text.split(","):
        part = raw_part.strip()
        if not part:
            continue
        if part.startswith("#"):
            add([chapter_by_index(chapters, part)])
            continue
        bounds = split_range(part)
        if bounds is not None:
            left, right = bounds
            matched = [
                chapter
                for chapter in chapters
                if chapter.number is not None and in_numeric_range(chapter.number, left, right)
            ]
            if not matched:
                raise ValueError(f"в диапазоне {part} нет глав")
            add(latest_per_slot(matched))
            continue
        matched = [chapter for chapter in chapters if same_number(part, chapter.number)]
        if not matched:
            raise ValueError(f"глава {part} не найдена")
        add(latest_per_slot(matched))
    if not picked:
        raise ValueError("ни одна глава не выбрана")
    return picked


def filter_volume(chapters: list[Chapter], volume: str | None) -> list[Chapter]:
    if not volume:
        return chapters
    matched = [chapter for chapter in chapters if same_number(volume, chapter.volume)]
    if not matched:
        raise ValueError(f"том {volume} не найден")
    return matched


def classify_target(text: str) -> tuple[str, str]:
    raw = text.strip()
    chapter = re.search(rf"mangadex\.org/chapter/({UUID_RE.pattern})", raw, re.IGNORECASE)
    if chapter:
        return "chapter", chapter.group(1)
    title = re.search(rf"mangadex\.org/title/({UUID_RE.pattern})", raw, re.IGNORECASE)
    if title:
        return "manga", title.group(1)
    if UUID_RE.fullmatch(raw):
        return "uuid", raw
    return "search", raw


def collect_titles(attributes: dict[str, Any]) -> dict[str, str]:
    titles: dict[str, str] = {}
    for lang, value in (attributes.get("title") or {}).items():
        if value:
            titles[lang] = value
    for alt in attributes.get("altTitles") or []:
        if not isinstance(alt, dict):
            continue
        for lang, value in alt.items():
            if value:
                titles.setdefault(lang, value)
    return titles


def choose_title(titles: dict[str, str], lang: str) -> str:
    for key in (lang, "en", "ja-ro", "ja", "ko", "zh"):
        if titles.get(key):
            return titles[key]
    if titles:
        return next(iter(titles.values()))
    return "manga"


def all_aliases(attributes: dict[str, Any]) -> tuple[str, ...]:
    found: list[str] = []
    for value in (attributes.get("title") or {}).values():
        if value:
            found.append(value)
    for alt in attributes.get("altTitles") or []:
        if isinstance(alt, dict):
            found.extend(value for value in alt.values() if value)
    return tuple(found)


def parse_manga(obj: dict[str, Any], lang: str) -> Manga:
    attributes = obj.get("attributes") or {}
    titles = collect_titles(attributes)
    primary = next(iter((attributes.get("title") or {"und": "manga"}).values()), "manga")
    languages = tuple(attributes.get("availableTranslatedLanguages") or [])
    year = attributes.get("year")
    return Manga(
        id=obj["id"],
        title=one_line(choose_title(titles, lang)),
        original_title=one_line(str(primary)),
        year=int(year) if isinstance(year, int) else None,
        status=attributes.get("status"),
        original_language=attributes.get("originalLanguage"),
        languages=languages,
        aliases=all_aliases(attributes),
    )


def parse_chapter(obj: dict[str, Any]) -> Chapter:
    attributes = obj.get("attributes") or {}
    groups: list[str] = []
    uploader = ""
    for relation in obj.get("relationships") or []:
        attrs = relation.get("attributes") or {}
        if relation.get("type") == "scanlation_group" and attrs.get("name"):
            groups.append(str(attrs["name"]))
        elif relation.get("type") == "user" and attrs.get("username") and not uploader:
            uploader = str(attrs["username"])
    number = attributes.get("chapter")
    volume = attributes.get("volume")
    title = attributes.get("title") or None
    credit = ", ".join(groups) if groups else uploader
    return Chapter(
        id=obj["id"],
        number=str(number) if number not in (None, "") else None,
        volume=str(volume) if volume not in (None, "") else None,
        title=one_line(title) if title else None,
        language=attributes.get("translatedLanguage") or "",
        pages=int(attributes.get("pages") or 0),
        group=one_line(credit) if credit else "",
        updated_at=attributes.get("updatedAt") or attributes.get("publishAt") or "",
        external_url=attributes.get("externalUrl") or None,
    )


def credit(chapter: Chapter) -> str:
    return chapter.group or "без группы"


def chapter_dirname(chapter: Chapter) -> str:
    number = (chapter.number or "").strip()
    if re.fullmatch(r"\d+", number):
        label = f"c{int(number):03d}"
    elif number:
        label = f"c{number.replace('/', '-')}"
    else:
        label = "c000-oneshot"
    if chapter.volume:
        label = f"v{chapter.volume}-{label}"
    if chapter.group:
        label += f" [{chapter.group}]"
    if chapter.title:
        label += f" - {chapter.title}"
    return sanitize(label, 80)


def chapter_dir(root: Path, manga: Manga, lang: str, chapter: Chapter) -> Path:
    return root / sanitize(manga.title, 60) / sanitize(lang, 16) / chapter_dirname(chapter)


def format_manga(index: int, manga: Manga) -> str:
    year = str(manga.year) if manga.year else "—"
    status = manga.status or "—"
    origin = manga.original_language or "—"
    extra = ""
    if manga.original_title and manga.original_title.casefold() != manga.title.casefold():
        extra = f" / {manga.original_title}"
    langs = ", ".join(manga.languages[:10]) or "—"
    return (
        f"{index:3}. {manga.title}{extra}  ({year}, {status}, {origin})  [{langs}]\n"
        f"     https://mangadex.org/title/{manga.id}"
    )


def format_chapter(index: int, chapter: Chapter) -> str:
    number = chapter.number or "oneshot"
    volume = f"том {chapter.volume}" if chapter.volume else "том —"
    title = f"  {chapter.title}" if chapter.title else ""
    external = "  внешняя ссылка" if chapter.external_url else ""
    return (
        f"{index:4}. глава {number:<8} {volume:<10} {chapter.pages:4} стр.  "
        f"{credit(chapter)}{title}{external}"
    )


def looks_like_image(data: bytes, content_type: str) -> bool:
    if len(data) < 16:
        return False
    if data.startswith((b"\x89PNG", b"\xff\xd8\xff", b"GIF8", b"RIFF")):
        return True
    if b"ftyp" in data[:32]:
        return True
    if content_type.lower().startswith("image/") and not data.startswith((b"<", b"{")):
        return True
    return False


def error_detail(payload: Any) -> str:
    if not isinstance(payload, dict):
        return "ошибка API MangaDex"
    parts: list[str] = []
    for err in payload.get("errors") or []:
        if isinstance(err, dict) and err.get("detail"):
            parts.append(str(err["detail"]))
    return "; ".join(parts) or "ошибка API MangaDex"


def rating_params() -> list[tuple[str, str]]:
    return [("contentRating[]", rating) for rating in CONTENT_RATINGS]


def retry_delay(headers: Any, attempt: int) -> float:
    raw = headers.get("Retry-After") if headers is not None else None
    if raw:
        try:
            return min(float(raw), 30.0)
        except (TypeError, ValueError):
            pass
    return min(8.0, 0.8 * (2**attempt))


class Client:
    def __init__(self) -> None:
        self.api_limiter = Limiter(0.25)
        self.image_limiter = Limiter(0.2)

    def get_json(self, url: str) -> dict[str, Any]:
        status, _headers, body = self._request(
            url,
            accept="application/json",
            limiter=self.api_limiter,
        )
        try:
            payload = json.loads(body.decode("utf-8"))
        except json.JSONDecodeError as exc:
            raise ApiError(f"ответ не JSON (HTTP {status})") from exc
        if isinstance(payload, dict) and payload.get("result") == "error":
            raise ApiError(error_detail(payload), status)
        if not isinstance(payload, dict):
            raise ApiError(f"неожиданный ответ API (HTTP {status})")
        return payload

    def _request(
        self,
        url: str,
        *,
        method: str = "GET",
        data: bytes | None = None,
        headers: dict[str, str] | None = None,
        accept: str | None = None,
        timeout: float = 60,
        limiter: Limiter | None = None,
        retries: int = 4,
    ) -> tuple[int, Any, bytes]:
        last_error: Exception | None = None
        for attempt in range(retries):
            if limiter is not None:
                limiter.wait()
            hdrs = {"User-Agent": USER_AGENT}
            if accept:
                hdrs["Accept"] = accept
            if headers:
                hdrs.update(headers)
            request = Request(url, data=data, headers=hdrs, method=method)
            try:
                with urlopen(request, timeout=timeout) as response:
                    return response.status, response.headers, response.read()
            except HTTPError as exc:
                body = exc.read()
                if exc.code in {429, 500, 502, 503, 504} and attempt + 1 < retries:
                    time.sleep(retry_delay(exc.headers, attempt))
                    last_error = exc
                    continue
                detail = exc.reason
                try:
                    parsed = json.loads(body.decode("utf-8"))
                    detail = error_detail(parsed)
                except (UnicodeDecodeError, json.JSONDecodeError):
                    if body:
                        detail = body[:200].decode("utf-8", errors="replace")
                raise ApiError(f"HTTP {exc.code}: {detail}", exc.code) from exc
            except URLError as exc:
                last_error = exc
                if attempt + 1 < retries:
                    time.sleep(1.5 * (attempt + 1))
                    continue
                raise ApiError(f"сеть: {exc.reason}") from exc
        raise ApiError(f"сеть: {last_error}")

    def get_manga(self, manga_id: str, lang: str) -> Manga:
        payload = self.get_json(f"{API}/manga/{manga_id}")
        return parse_manga(payload["data"], lang)

    def search(self, title: str, lang: str) -> tuple[list[Manga], int]:
        params: list[tuple[str, str]] = [
            ("title", title),
            ("limit", "15"),
            ("order[relevance]", "desc"),
            *rating_params(),
        ]
        payload = self.get_json(f"{API}/manga?{urlencode(params)}")
        found = [parse_manga(item, lang) for item in payload.get("data") or []]
        return found, int(payload.get("total") or len(found))

    def chapters(self, manga_id: str, lang: str) -> list[Chapter]:
        found: list[Chapter] = []
        offset = 0
        total: int | None = None
        while total is None or offset < total:
            params: list[tuple[str, str]] = [
                ("limit", "100"),
                ("offset", str(offset)),
                ("translatedLanguage[]", lang),
                ("order[volume]", "asc"),
                ("order[chapter]", "asc"),
                ("includeFuturePublishAt", "0"),
                ("includeEmptyPages", "0"),
                ("includes[]", "scanlation_group"),
                ("includes[]", "user"),
                *rating_params(),
            ]
            payload = self.get_json(f"{API}/manga/{manga_id}/feed?{urlencode(params)}")
            batch = payload.get("data") or []
            total = int(payload.get("total") or 0)
            found.extend(parse_chapter(item) for item in batch)
            offset += len(batch)
            if not batch:
                break
        return sort_chapters(found)

    def chapter_with_manga(self, chapter_id: str, lang: str) -> tuple[Manga, Chapter]:
        params = [
            ("includes[]", "scanlation_group"),
            ("includes[]", "user"),
            ("includes[]", "manga"),
        ]
        payload = self.get_json(f"{API}/chapter/{chapter_id}?{urlencode(params)}")
        obj = payload["data"]
        chapter = parse_chapter(obj)
        manga_obj = None
        manga_id = None
        for relation in obj.get("relationships") or []:
            if relation.get("type") != "manga":
                continue
            manga_id = relation.get("id")
            if relation.get("attributes"):
                manga_obj = relation
            break
        if manga_obj is not None:
            return parse_manga(manga_obj, lang), chapter
        if not manga_id:
            raise ApiError("у главы нет связанного тайтла")
        return self.get_manga(manga_id, lang), chapter

    def at_home(self, chapter_id: str) -> dict[str, Any]:
        payload = self.get_json(f"{API}/at-home/server/{chapter_id}?forcePort443=true")
        chapter = payload.get("chapter") or {}
        if not payload.get("baseUrl") or not chapter.get("hash"):
            raise ApiError("MangaDex не выдал сервер для страниц")
        return payload

    def fetch_image(self, url: str) -> tuple[bool, bytes, Any, int]:
        self.image_limiter.wait()
        started = time.monotonic()
        request = Request(
            url,
            headers={
                "User-Agent": USER_AGENT,
                "Referer": "https://mangadex.org/",
                "Accept": "image/avif,image/webp,image/png,image/jpeg,*/*",
            },
        )
        try:
            with urlopen(request, timeout=120) as response:
                data = response.read()
                elapsed = int((time.monotonic() - started) * 1000)
                content_type = response.headers.get("Content-Type") or ""
                ok = response.status == 200 and looks_like_image(data, content_type)
                return ok, data, response.headers, elapsed
        except HTTPError as exc:
            data = exc.read() or b""
            elapsed = int((time.monotonic() - started) * 1000)
            return False, data, exc.headers, elapsed
        except URLError:
            elapsed = int((time.monotonic() - started) * 1000)
            return False, b"", None, elapsed

    def report(self, url: str, success: bool, nbytes: int, duration: int, cached: bool) -> None:
        host = urlparse(url).netloc.lower()
        if "mangadex.org" in host:
            return
        payload = json.dumps(
            {
                "url": url,
                "success": success,
                "cached": cached,
                "bytes": nbytes,
                "duration": duration,
            }
        ).encode("utf-8")
        try:
            self._request(
                REPORT_URL,
                method="POST",
                data=payload,
                headers={"Content-Type": "application/json"},
                timeout=15,
                limiter=self.api_limiter,
                retries=1,
            )
        except (ApiError, OSError):
            return


def prompt(text: str) -> str:
    try:
        return input(text).strip()
    except EOFError:
        print()
        return ""


def is_tty() -> bool:
    return sys.stdin.isatty() and sys.stdout.isatty()


def validate_lang(lang: str) -> str:
    cleaned = lang.strip().lower().replace("_", "-")
    if not _LANG_RE.fullmatch(cleaned):
        raise ValueError(f"неизвестный код языка: {lang}")
    return cleaned


def choose_manga(client: Client, query: str, lang: str, pick: int | None) -> Manga:
    found, total = client.search(query, lang)
    if not found:
        raise ValueError(f"тайтл «{query}» не найден")
    if pick is not None:
        if pick < 1 or pick > len(found):
            raise ValueError(f"нет тайтла с номером {pick}, в списке {len(found)}")
        return found[pick - 1]
    exact = [
        manga
        for manga in found
        if any(alias.casefold() == query.casefold() for alias in manga.aliases)
    ]
    if len(exact) == 1:
        return exact[0]
    if len(found) == 1:
        return found[0]
    if total > len(found):
        print(f"Показаны первые {len(found)} из {total}. Если нужного нет, вставьте ссылку на тайтл.")
    print(f"Найдено совпадений: {total}. Уточните номер.")
    for index, manga in enumerate(found, 1):
        print(format_manga(index, manga))
    if not is_tty():
        raise ValueError("несколько тайтлов, укажите --pick N или ссылку на тайтл")
    raw = prompt("Номер тайтла: ")
    if not raw.isdigit():
        raise ValueError("нужен номер тайтла из списка")
    number = int(raw)
    if number < 1 or number > len(found):
        raise ValueError(f"нет тайтла с номером {number}")
    return found[number - 1]


def load_chapters(client: Client, manga: Manga, lang: str) -> tuple[str, list[Chapter]]:
    current = lang
    while True:
        chapters = client.chapters(manga.id, current)
        if chapters:
            return current, chapters
        available = ", ".join(manga.languages) or "нет данных"
        print(f"На языке {current} глав нет. Доступные языки: {available}")
        if not is_tty():
            raise ValueError(f"на языке {current} глав нет")
        raw = prompt("Другой язык (пусто — выход): ")
        if not raw:
            return current, []
        current = validate_lang(raw)


def ask_spec(chapters: list[Chapter]) -> str:
    print(f"Глав в списке: {len(chapters)}")
    for index, chapter in enumerate(chapters, 1):
        print(format_chapter(index, chapter))
    raw = prompt("Какие главы скачать? (12, 1-5, 1,3, all, #2): ")
    if not raw:
        raise ValueError("глава не выбрана")
    return raw


def note_alternatives(chapters: list[Chapter], selected: list[Chapter], spec: str) -> None:
    if "#" in spec:
        return
    index = {chapter.id: number for number, chapter in enumerate(chapters, 1)}
    for chapter in selected:
        siblings = [
            other
            for other in chapters
            if other.id != chapter.id
            and (other.volume or "") == (chapter.volume or "")
            and same_number(chapter.number or "oneshot", other.number)
        ]
        if not siblings:
            continue
        others = ", ".join(f"#{index[item.id]} {credit(item)}" for item in siblings)
        label = chapter.number or "oneshot"
        print(f"Глава {label}: скачиваю самый свежий перевод «{credit(chapter)}». Другие: {others}")


def cached_header(headers: Any) -> bool:
    if headers is None:
        return False
    value = headers.get("X-Cache") or ""
    return str(value).upper().startswith("HIT")


def download_chapter(
    client: Client,
    manga: Manga,
    lang: str,
    chapter: Chapter,
    root: Path,
    *,
    saver: bool,
    force: bool,
) -> DownloadResult:
    label = chapter.number or "oneshot"
    if chapter.external_url:
        print(f"Глава {label} лежит вне MangaDex: {chapter.external_url}")
        return DownloadResult(failed=1)
    if chapter.pages == 0:
        print(f"Глава {label}: на MangaDex нет страниц")
        return DownloadResult(failed=1)

    destination = chapter_dir(root, manga, lang, chapter)
    destination.mkdir(parents=True, exist_ok=True)
    quality = "data-saver" if saver else "data"
    file_key = "dataSaver" if saver else "data"
    server = client.at_home(chapter.id)
    filenames = list((server.get("chapter") or {}).get(file_key) or [])
    if not filenames:
        print(f"Глава {label}: сервер не отдал список страниц")
        return DownloadResult(failed=1)

    result = DownloadResult()
    total = len(filenames)
    for index, filename in enumerate(filenames, 1):
        suffix = Path(filename).suffix or ".img"
        target = destination / f"{index:03d}{suffix}"
        if target.exists() and target.stat().st_size > 0 and not force:
            result.skipped += 1
            line = f"  {index}/{total} уже есть"
            if sys.stdout.isatty():
                print(f"\r{line}          ", end="", flush=True)
            else:
                print(line)
            continue
        saved = False
        for _attempt in range(3):
            base = str(server["baseUrl"]).rstrip("/")
            chapter_hash = server["chapter"]["hash"]
            current_name = filename
            url = f"{base}/{quality}/{chapter_hash}/{quote(current_name)}"
            ok, data, headers, elapsed = client.fetch_image(url)
            client.report(url, ok, len(data), elapsed, cached_header(headers))
            if ok:
                partial = target.with_suffix(target.suffix + ".part")
                partial.write_bytes(data)
                partial.replace(target)
                result.downloaded += 1
                saved = True
                if sys.stdout.isatty():
                    print(f"\r  {index}/{total} {target.name}          ", end="", flush=True)
                else:
                    print(f"  {index}/{total} {target.name}")
                break
            server = client.at_home(chapter.id)
            refreshed = list((server.get("chapter") or {}).get(file_key) or [])
            if len(refreshed) == total:
                filename = refreshed[index - 1]
        if not saved:
            print(f"\nГлава {label}, страница {index}: не скачалась")
            result.failed += 1
    if sys.stdout.isatty():
        print()
    print(
        f"Глава {label}: новых {result.downloaded}, "
        f"уже было {result.skipped}, ошибок {result.failed} → {destination}"
    )
    return result


def resolve_direct_chapter(client: Client, chapter_id: str, lang: str) -> tuple[Manga, list[Chapter], str]:
    manga, chapter = client.chapter_with_manga(chapter_id, lang)
    use_lang = chapter.language or lang
    return manga, [chapter], use_lang


def print_chapter_list(chapters: list[Chapter]) -> None:
    print(f"Глав: {len(chapters)}")
    for index, chapter in enumerate(chapters, 1):
        print(format_chapter(index, chapter))


def run(args: argparse.Namespace) -> int:
    lang = validate_lang(args.lang)
    client = Client()
    target = (args.target or "").strip()
    if not target:
        if not is_tty():
            raise ValueError("укажите название или ссылку MangaDex")
        target = prompt("Название или ссылка MangaDex: ")
        if not target:
            raise ValueError("пустой запрос")

    kind, value = classify_target(target)
    direct_chapters: list[Chapter] | None = None
    if kind == "chapter":
        manga, direct_chapters, lang = resolve_direct_chapter(client, value, lang)
    elif kind == "manga":
        manga = client.get_manga(value, lang)
    elif kind == "uuid":
        try:
            manga = client.get_manga(value, lang)
        except ApiError as exc:
            if exc.status != 404:
                raise
            manga, direct_chapters, lang = resolve_direct_chapter(client, value, lang)
    else:
        manga = choose_manga(client, value, lang, args.pick)

    print(f"Тайтл: {manga.title}")
    print(f"Страница: https://mangadex.org/title/{manga.id}")

    if direct_chapters is None:
        lang, chapters = load_chapters(client, manga, lang)
        if not chapters:
            return 1
        chapters = filter_volume(chapters, args.volume)
        if args.list:
            print_chapter_list(chapters)
            return 0
        spec = (args.chapter or "").strip()
        if not spec:
            if not is_tty():
                raise ValueError("укажите главы через -c, например -c 12 или -c 1-5")
            spec = ask_spec(chapters)
        selected = select_chapters(chapters, spec)
        note_alternatives(chapters, selected, spec)
    else:
        selected = direct_chapters
        if args.list:
            print_chapter_list(selected)
            return 0

    root = Path(args.out).expanduser().resolve()
    print(f"Язык: {lang}")
    print(f"К сохранению: {len(selected)}")
    print(f"Папка: {root}")
    totals = DownloadResult()
    for chapter in selected:
        label = chapter.number or "oneshot"
        print(f"Скачиваю главу {label} [{credit(chapter)}]")
        try:
            result = download_chapter(
                client,
                manga,
                lang,
                chapter,
                root,
                saver=args.saver,
                force=args.force,
            )
        except ApiError as exc:
            print(f"Глава {label}: {exc}", file=sys.stderr)
            totals.failed += 1
            continue
        totals.downloaded += result.downloaded
        totals.skipped += result.skipped
        totals.failed += result.failed
    print(
        f"Готово. Новых страниц: {totals.downloaded}, "
        f"пропущено: {totals.skipped}, ошибок: {totals.failed}"
    )
    return 1 if totals.failed else 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="mangadex-dl",
        description=(
            "Скачивает страницы выбранных глав с mangadex.org "
            "для личного некоммерческого чтения. Windows и Linux."
        ),
        epilog=(
            "примеры:\n"
            "  python mangadex_dl.py\n"
            '  python mangadex_dl.py "Ёцуба" -c 1 -l ru\n'
            '  python mangadex_dl.py "https://mangadex.org/title/UUID" -c 1-5 -l en\n'
            '  python mangadex_dl.py "https://mangadex.org/chapter/UUID"\n'
            "\n"
            "Глава: 12 | 1-5 | 1,3,8 | all | #2 (строка из списка).\n"
            "Повторный запуск пропускает уже скачанные страницы."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("target", nargs="?", help="название, ссылка на тайтл или ссылка на главу")
    parser.add_argument("-c", "--chapter", help="какие главы скачать")
    parser.add_argument("-l", "--lang", default="ru", help="язык перевода, по умолчанию ru")
    parser.add_argument("-v", "--volume", help="только этот том")
    parser.add_argument("-o", "--out", default="downloads", help="папка для страниц, по умолчанию downloads")
    parser.add_argument("--saver", action="store_true", help="сжатые страницы data-saver")
    parser.add_argument("--list", action="store_true", help="показать главы и выйти")
    parser.add_argument("--force", action="store_true", help="скачать страницы заново")
    parser.add_argument("--pick", type=int, help="номер тайтла в результатах поиска, с 1")
    parser.add_argument("--version", action="version", version="mangadex-dl 1.0.0")
    return parser


def main(argv: list[str] | None = None) -> int:
    configure_stdio()
    args = build_parser().parse_args(argv)
    try:
        return run(args)
    except KeyboardInterrupt:
        print("\nОстановлено")
        return 130
    except BrokenPipeError:
        try:
            sys.stdout.close()
        except BrokenPipeError:
            pass
        with open(os.devnull, "w", encoding="utf-8") as sink:
            os.dup2(sink.fileno(), 1)
        return 0
    except (ApiError, ValueError, OSError) as exc:
        print(f"Ошибка: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
