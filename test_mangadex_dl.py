import unittest

from mangadex_dl import (
    Chapter,
    chapter_dirname,
    choose_title,
    classify_target,
    latest_per_slot,
    parse_chapter,
    sanitize,
    same_number,
    select_chapters,
)


def chapter(**kwargs: object) -> Chapter:
    data: dict[str, object] = {
        "id": "id",
        "number": "1",
        "volume": None,
        "title": None,
        "language": "ru",
        "pages": 10,
        "group": "Group",
        "updated_at": "2020-01-01T00:00:00+00:00",
        "external_url": None,
    }
    data.update(kwargs)
    return Chapter(**data)  # type: ignore[arg-type]


class SanitizeTests(unittest.TestCase):
    def test_strips_windows_illegal_characters(self) -> None:
        self.assertEqual(sanitize('a<b>:c/d\\e|f?g*h"'), "a b c d e f g h")

    def test_reserves_windows_device_names(self) -> None:
        self.assertEqual(sanitize("CON"), "_CON")

    def test_trims_trailing_dot_and_length(self) -> None:
        self.assertEqual(sanitize("name."), "name")
        self.assertEqual(len(sanitize("я" * 80, limit=12)), 12)


class TargetTests(unittest.TestCase):
    def test_classifies_title_chapter_and_search(self) -> None:
        title = "https://mangadex.org/title/58be6aa6-06cb-4ca5-bd20-f1392ce451fb/yotsuba"
        chapter_url = "https://mangadex.org/chapter/abb708e6-66cc-4a77-8255-a42008da2f70"
        self.assertEqual(classify_target(title)[0], "manga")
        self.assertEqual(classify_target(chapter_url)[0], "chapter")
        self.assertEqual(classify_target("Ёцуба"), ("search", "Ёцуба"))
        self.assertEqual(
            classify_target("58be6aa6-06cb-4ca5-bd20-f1392ce451fb")[0],
            "uuid",
        )


class TitleTests(unittest.TestCase):
    def test_prefers_requested_language(self) -> None:
        titles = {"ja-ro": "Yotsuba to!", "en": "Yotsuba&!", "ru": "Ёцуба!"}
        self.assertEqual(choose_title(titles, "ru"), "Ёцуба!")
        self.assertEqual(choose_title(titles, "en"), "Yotsuba&!")


class ChapterSelectionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.chapters = [
            chapter(id="a", number="1", group="Old", updated_at="2020-01-01T00:00:00+00:00"),
            chapter(id="b", number="1", group="New", updated_at="2024-01-01T00:00:00+00:00"),
            chapter(id="c", number="2", volume="1", group="Vol"),
            chapter(id="d", number="12.5", group="Half"),
            chapter(id="e", number=None, group="Oneshot"),
        ]

    def test_single_number_picks_latest_group(self) -> None:
        picked = select_chapters(self.chapters, "1")
        self.assertEqual([item.id for item in picked], ["b"])

    def test_decimal_does_not_match_integer(self) -> None:
        self.assertFalse(same_number("12", "12.5"))
        self.assertTrue(same_number("12.50", "12.5"))
        with self.assertRaises(ValueError):
            select_chapters(self.chapters, "12")

    def test_range_includes_decimal_and_keeps_volumes(self) -> None:
        picked = select_chapters(self.chapters, "1-12.5")
        self.assertEqual([item.id for item in picked], ["b", "c", "d"])

    def test_list_oneshot_and_index(self) -> None:
        self.assertEqual([item.id for item in select_chapters(self.chapters, "2, oneshot")], ["c", "e"])
        self.assertEqual(select_chapters(self.chapters, "#1")[0].id, "a")

    def test_all_collapses_duplicate_groups_only(self) -> None:
        picked = latest_per_slot(self.chapters)
        self.assertEqual([item.id for item in picked], ["b", "c", "d", "e"])

    def test_uploader_is_used_when_there_is_no_group(self) -> None:
        parsed = parse_chapter(
            {
                "id": "x",
                "attributes": {"chapter": "1", "pages": 2, "translatedLanguage": "ru"},
                "relationships": [{"type": "user", "attributes": {"username": "az228"}}],
            }
        )
        self.assertEqual(parsed.group, "az228")
        self.assertEqual(chapter_dirname(chapter(number="4", group="")), "c004")

    def test_folder_name_is_padded_and_safe(self) -> None:
        name = chapter_dirname(
            chapter(number="3", volume="2", group="A/B", title='Том: "старт"')
        )
        self.assertTrue(name.startswith("v2-c003"))
        self.assertNotIn("/", name)
        self.assertNotIn(":", name)


if __name__ == "__main__":
    unittest.main()
