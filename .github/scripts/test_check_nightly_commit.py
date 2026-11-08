#!/usr/bin/env python3

import unittest

from check_nightly_commit import check
from check_nightly_commit import escape

MAIN = "refs/heads/main"
# Main's real history around the 10-29..11-06 nightlies (#1812), newest
# first as the Activity API returns it.
ACTIVITIES = [
    {"timestamp": "2026-11-06T18:52:42Z", "after": "2d8c17d66a"},
    {"timestamp": "2026-10-29T10:53:04Z", "after": "1329ea5883"},
    {"timestamp": "2026-10-29T10:33:18Z", "after": "3d2f79653a"},
]


class CheckNightlyCommitTest(unittest.TestCase):
    def levels(self, annotations: list[tuple[str, str]]) -> list[str]:
        return [level for level, _ in annotations]

    def test_run_of_main_head_passes(self) -> None:
        code, annotations = check(
            MAIN, "1329ea5883", "2026-10-29T16:14:26Z", ACTIVITIES
        )
        self.assertEqual(code, 0)
        self.assertEqual(self.levels(annotations), ["notice", "warning"])
        # main moved later, so the run says its results describe the old head.
        self.assertIn("main moved to 2d8c17d66a", annotations[1][1])

    def test_unchanged_main_warns_that_the_run_retests_old_code(self) -> None:
        code, annotations = check(
            MAIN, "1329ea5883", "2026-11-06T16:49:19Z", ACTIVITIES[1:]
        )
        self.assertEqual(code, 0)
        self.assertEqual(self.levels(annotations), ["notice", "warning"])
        self.assertIn(
            "main hasn't changed since 2026-10-29T10:53:04Z", annotations[1][1]
        )
        self.assertIn("8 d 5 h", annotations[1][1])

    def test_fresh_head_has_no_warning(self) -> None:
        code, annotations = check(
            MAIN, "2d8c17d66a", "2026-11-06T20:00:00Z", ACTIVITIES
        )
        self.assertEqual((code, self.levels(annotations)), (0, ["notice"]))

    def test_commit_that_was_not_main_head_fails(self) -> None:
        # Made up: the real run created then tested dfeb26a122.
        code, annotations = check(
            MAIN, "1329ea5883", "2026-11-07T15:44:22Z", ACTIVITIES
        )
        self.assertEqual((code, self.levels(annotations)), (1, ["error"]))
        self.assertIn("main's head was 2d8c17d66a", annotations[0][1])

    def test_earlier_main_commit_fails(self) -> None:
        code, _ = check(MAIN, "3d2f79653a", "2026-10-30T00:00:00Z", ACTIVITIES)
        self.assertEqual(code, 1)

    def test_unknown_head_at_creation_warns(self) -> None:
        code, annotations = check(
            MAIN, "3d2f79653a", "2026-10-01T00:00:00Z", ACTIVITIES
        )
        self.assertEqual((code, self.levels(annotations)), (0, ["warning"]))

    def test_same_second_updates_keep_the_api_order(self) -> None:
        activities = [
            {"timestamp": "2026-11-07T10:00:00Z", "after": "newer"},
            {"timestamp": "2026-11-07T10:00:00Z", "after": "older"},
        ]
        code, annotations = check(MAIN, "newer", "2026-11-07T12:00:00Z", activities)
        self.assertEqual((code, self.levels(annotations)), (0, ["notice"]))

    def test_push_just_before_creation_only_warns(self) -> None:
        # GitHub may have picked the previous head for a run created a
        # moment after a push.
        code, annotations = check(
            MAIN, "1329ea5883", "2026-11-06T18:53:30Z", ACTIVITIES
        )
        self.assertEqual((code, self.levels(annotations)), (0, ["warning"]))
        self.assertIn("main moved to 2d8c17d66a", annotations[0][1])

    def test_no_main_history_fails(self) -> None:
        code, annotations = check(MAIN, "1329ea5883", "2026-11-06T16:49:19Z", [])
        self.assertEqual((code, self.levels(annotations)), (1, ["error"]))

    def test_escape(self) -> None:
        self.assertEqual(escape("50%\r\nx"), "50%25%0D%0Ax")

    def test_branch_run_is_a_notice(self) -> None:
        code, annotations = check(
            "refs/heads/ci-test", "abc", "2026-11-07T00:00:00Z", ACTIVITIES
        )
        self.assertEqual((code, self.levels(annotations)), (0, ["notice"]))
        self.assertIn("main's head: 2d8c17d66a", annotations[0][1])


if __name__ == "__main__":
    unittest.main()
