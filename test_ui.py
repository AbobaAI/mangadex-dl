import json
import threading
import unittest
from http.client import HTTPConnection
from pathlib import Path

from mangadex_dl import Chapter, Manga, build_parser, wants_ui
from ui_server import App, Server, make_handler


def manga() -> Manga:
    return Manga(
        id="58be6aa6-06cb-4ca5-bd20-f1392ce451fb",
        title="Ёцуба!",
        original_title="Yotsuba to!",
        year=2003,
        status="ongoing",
        original_language="ja",
        languages=("ru", "en"),
        aliases=("Ёцуба!", "Yotsuba to!"),
        cover=None,
    )


def chapters() -> list[Chapter]:
    return [
        Chapter("old", "1", "1", None, "ru", 10, "Old", "2020-01-01T00:00:00+00:00", None),
        Chapter("new", "1", "1", None, "ru", 12, "New", "2024-01-01T00:00:00+00:00", None),
        Chapter("two", "2", "1", "Прогулка", "ru", 8, "New", "2024-02-01T00:00:00+00:00", None),
    ]


class FakeClient:
    def search(self, title: str, lang: str) -> tuple[list[Manga], int]:
        return [manga()], 1

    def get_manga(self, manga_id: str, lang: str) -> Manga:
        found = manga()
        found.id = manga_id
        return found

    def chapters(self, manga_id: str, lang: str) -> list[Chapter]:
        return chapters()

    def chapter_with_manga(self, chapter_id: str, lang: str) -> tuple[Manga, Chapter]:
        return manga(), chapters()[1]


class InterfaceTests(unittest.TestCase):
    def test_plain_launch_opens_the_interface(self) -> None:
        self.assertTrue(wants_ui(build_parser().parse_args([])))
        self.assertFalse(wants_ui(build_parser().parse_args(["--cli"])))
        self.assertFalse(wants_ui(build_parser().parse_args(["Ёцуба", "-c", "1"])))

    def test_pages_search_and_chapter_marks(self) -> None:
        app = App(client=FakeClient(), out=Path("/tmp/mangadex-dl-ui"))
        server = Server(("127.0.0.1", 0), make_handler(app))
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        port = server.server_address[1]
        home = HTTPConnection("127.0.0.1", port)
        home.request("GET", "/")
        page = home.getresponse()
        body = page.read().decode("utf-8")
        self.assertEqual(page.status, 200)
        self.assertIn("Страницы выбранных глав", body)
        self.assertIn("prefers-color-scheme: dark", body)
        home.close()

        found = self._post(port, "/api/search", {"query": "Ёцуба!", "lang": "ru"})
        self.assertEqual(found["manga"]["title"], "Ёцуба!")
        self.assertEqual(len(found["chapters"]), 3)

        opened = self._post(port, "/api/open", {"id": "other-id", "lang": "ru"})
        self.assertEqual(opened["manga"]["id"], "other-id")
        self.assertEqual([item["id"] for item in opened["results"]], ["other-id"])

        marked = self._post(port, "/api/match", {"spec": "1"})
        self.assertEqual(marked["ids"], ["new"])
        missing = self._post(port, "/api/match", {"spec": "99"}, ok=False)
        self.assertIn("не найдена", missing["error"])

    def _post(self, port: int, path: str, payload: dict, ok: bool = True) -> dict:
        connection = HTTPConnection("127.0.0.1", port)
        raw = json.dumps(payload).encode("utf-8")
        connection.request("POST", path, body=raw, headers={"Content-Type": "application/json"})
        response = connection.getresponse()
        data = json.loads(response.read().decode("utf-8"))
        connection.close()
        self.assertEqual(response.status, 200 if ok else 400)
        return data


if __name__ == "__main__":
    unittest.main()
