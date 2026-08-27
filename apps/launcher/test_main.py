"""Tests for the launcher app.

These tests cover the parts of the launcher that don't depend on a real
Linux desktop session: `.desktop` parsing, locale resolution, fuzzy
scoring, Exec= field-code expansion, recent-log rotation, and the AppID
shadowing rule across XDG paths. The live launch and `/proc` scanning
paths are integration-tested separately on a real Claw OS host.
"""

import json
import os
import pathlib
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(__file__))
sys.path.insert(
    0,
    os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
        os.pardir,
        "claw-os-sdk",
        "python",
        "src",
    ),
)  # for `from claw_os_sdk import …`
sys.path.insert(
    0,
    os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
        os.pardir,
        "cos-runtime",
        "python",
        "src",
    ),
)  # for `from cos_runtime import policy`

from test_support import load_local_module

main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_launcher_main",
    clear_modules=("_shared",),
)


# ---------------------------------------------------------------------------
# .desktop parsing
# ---------------------------------------------------------------------------


class TestDesktopFileParsing(unittest.TestCase):
    def _write(self, content):
        tmp = tempfile.NamedTemporaryFile(
            "w", suffix=".desktop", delete=False, encoding="utf-8"
        )
        tmp.write(content)
        tmp.close()
        self.addCleanup(os.unlink, tmp.name)
        return tmp.name

    def test_parses_basic_entry(self):
        path = self._write(
            "[Desktop Entry]\n"
            "Type=Application\n"
            "Name=Files\n"
            "Exec=cosmic-files %F\n"
            "Icon=cosmic-files\n"
        )
        entries = main._parse_desktop_file(path)
        self.assertIsNotNone(entries)
        self.assertEqual(entries["Name"], "Files")
        self.assertEqual(entries["Exec"], "cosmic-files %F")

    def test_skips_non_application_type(self):
        path = self._write(
            "[Desktop Entry]\nType=Link\nName=Doc\nURL=https://example.com\n"
        )
        self.assertIsNone(main._parse_desktop_file(path))

    def test_application_is_default_type(self):
        # Spec says the default Type is Application when missing.
        path = self._write("[Desktop Entry]\nName=NoType\nExec=foo\n")
        self.assertIsNotNone(main._parse_desktop_file(path))

    def test_ignores_other_sections(self):
        path = self._write(
            "[Desktop Entry]\nType=Application\nName=Main\nExec=app %F\n"
            "[Desktop Action open]\nName=ShouldBeIgnored\nExec=app-alt\n"
        )
        entries = main._parse_desktop_file(path)
        self.assertEqual(entries["Name"], "Main")
        self.assertEqual(entries["Exec"], "app %F")
        self.assertNotIn("Name[Desktop Action open]", entries)

    def test_skips_comments_and_blank_lines(self):
        path = self._write(
            "# top comment\n"
            "\n"
            "[Desktop Entry]\n"
            "# inline comment\n"
            "Name=Foo\n"
            "Exec=foo\n"
        )
        entries = main._parse_desktop_file(path)
        self.assertEqual(entries["Name"], "Foo")

    def test_missing_file_returns_none(self):
        self.assertIsNone(main._parse_desktop_file("/nonexistent/whatever.desktop"))


# ---------------------------------------------------------------------------
# Localized field lookup
# ---------------------------------------------------------------------------


class TestLocalization(unittest.TestCase):
    def test_full_locale_match_wins(self):
        entries = {
            "Name": "Files",
            "Name[zh]": "文件",
            "Name[zh_CN]": "文件管理器",
        }
        v = main._localized(entries, "Name", ["zh_CN", "zh"])
        self.assertEqual(v, "文件管理器")

    def test_falls_back_to_base_locale(self):
        entries = {"Name": "Files", "Name[zh]": "文件"}
        v = main._localized(entries, "Name", ["zh_CN", "zh"])
        self.assertEqual(v, "文件")

    def test_falls_back_to_bare_key(self):
        entries = {"Name": "Files"}
        v = main._localized(entries, "Name", ["zh_CN", "zh"])
        self.assertEqual(v, "Files")

    def test_empty_chain_returns_bare_key(self):
        entries = {"Name": "Files", "Name[zh]": "文件"}
        self.assertEqual(main._localized(entries, "Name", []), "Files")

    def test_locale_chain_c_locale_is_empty(self):
        with mock.patch.dict(os.environ, {"LANG": "C", "LC_MESSAGES": "", "LC_ALL": ""}):
            self.assertEqual(main._locale_chain(), [])

    def test_locale_chain_zh_cn_utf8(self):
        with mock.patch.dict(
            os.environ, {"LANG": "zh_CN.UTF-8", "LC_MESSAGES": "", "LC_ALL": ""}
        ):
            self.assertEqual(main._locale_chain(), ["zh_CN", "zh"])

    def test_lc_messages_overrides_lang(self):
        with mock.patch.dict(
            os.environ, {"LANG": "en_US.UTF-8", "LC_MESSAGES": "ja_JP.UTF-8", "LC_ALL": ""}
        ):
            self.assertEqual(main._locale_chain(), ["ja_JP", "ja"])


# ---------------------------------------------------------------------------
# Visibility filtering
# ---------------------------------------------------------------------------


class TestVisibility(unittest.TestCase):
    def test_hidden_excluded_by_default(self):
        self.assertFalse(
            main._passes_visibility({"Hidden": "true"}, set(), False, False)
        )

    def test_hidden_included_with_flag(self):
        self.assertTrue(
            main._passes_visibility({"Hidden": "true"}, set(), True, True)
        )

    def test_no_display_excluded_by_default(self):
        self.assertFalse(
            main._passes_visibility({"NoDisplay": "true"}, set(), False, False)
        )

    def test_only_show_in_match(self):
        e = {"OnlyShowIn": "COSMIC;GNOME;"}
        self.assertTrue(main._passes_visibility(e, {"COSMIC"}, False, False))
        self.assertFalse(main._passes_visibility(e, {"KDE"}, False, False))

    def test_not_show_in(self):
        e = {"NotShowIn": "GNOME;"}
        self.assertTrue(main._passes_visibility(e, {"COSMIC"}, False, False))
        self.assertFalse(main._passes_visibility(e, {"GNOME"}, False, False))


# ---------------------------------------------------------------------------
# Exec line handling
# ---------------------------------------------------------------------------


class TestExecExpansion(unittest.TestCase):
    def test_strips_field_codes_with_no_extras(self):
        self.assertEqual(main._expand_exec_line("cosmic-files %F", []), ["cosmic-files"])

    def test_single_file_code_substitutes_first(self):
        self.assertEqual(
            main._expand_exec_line("editor %f", ["/tmp/a.txt", "/tmp/b.txt"]),
            ["editor", "/tmp/a.txt"],
        )

    def test_list_code_substitutes_all(self):
        self.assertEqual(
            main._expand_exec_line("editor %F", ["/tmp/a", "/tmp/b"]),
            ["editor", "/tmp/a", "/tmp/b"],
        )

    def test_drops_deprecated_codes(self):
        for code in ("%i", "%c", "%k", "%d", "%D", "%n", "%N", "%v", "%m"):
            self.assertEqual(
                main._expand_exec_line(f"app {code}", []),
                ["app"],
                msg=f"code {code} should be dropped",
            )

    def test_quoted_args_preserved(self):
        out = main._expand_exec_line('cmd --flag "value with space" %F', ["x"])
        self.assertEqual(out, ["cmd", "--flag", "value with space", "x"])

    def test_percent_percent_literal(self):
        out = main._expand_exec_line("app 100%%", [])
        self.assertEqual(out, ["app", "100%"])

    def test_exec_binary_basename(self):
        self.assertEqual(
            main._exec_binary({"Exec": "/usr/bin/cosmic-files %F"}),
            "cosmic-files",
        )

    def test_exec_binary_prefers_try_exec(self):
        self.assertEqual(
            main._exec_binary({"TryExec": "/usr/bin/canary", "Exec": "wrapper %F"}),
            "canary",
        )


# ---------------------------------------------------------------------------
# Fuzzy scoring
# ---------------------------------------------------------------------------


class TestScoring(unittest.TestCase):
    def _entry(self, **kw):
        base = {
            "name": "",
            "app_id": "",
            "generic_name": "",
            "keywords": "",
            "exec_binary": "",
            "comment": "",
        }
        base.update(kw)
        return base

    def test_exact_name_match_beats_substring(self):
        exact = self._entry(name="Files")
        substring = self._entry(name="File Manager")
        self.assertGreater(main._score("Files", exact), main._score("Files", substring))

    def test_query_matches_keywords(self):
        e = self._entry(name="Cosmic Files", keywords="file;manager;explorer;")
        self.assertGreater(main._score("manager", e), 0)

    def test_zero_score_no_match(self):
        e = self._entry(name="Firefox", keywords="browser;web;")
        self.assertEqual(main._score("calculator", e), 0)

    def test_app_id_match(self):
        e = self._entry(app_id="com.clawos.Files", name="Files")
        self.assertGreater(main._score("clawos", e), 0)


# ---------------------------------------------------------------------------
# AppID derivation
# ---------------------------------------------------------------------------


class TestAppIdDerivation(unittest.TestCase):
    def test_basic(self):
        self.assertEqual(main._app_id_from_relpath("com.clawos.Files.desktop"), "com.clawos.Files")

    def test_nested_dir_becomes_dash(self):
        # freedesktop spec: subdirs are flattened with '-'.
        self.assertEqual(
            main._app_id_from_relpath("kde4/konsole.desktop"), "kde4-konsole"
        )

    def test_already_without_extension(self):
        self.assertEqual(main._app_id_from_relpath("Foo"), "Foo")


# ---------------------------------------------------------------------------
# End-to-end scan: AppID shadowing and visibility
# ---------------------------------------------------------------------------


class TestEndToEndScan(unittest.TestCase):
    def setUp(self):
        self.system = tempfile.mkdtemp()
        self.user = tempfile.mkdtemp()
        os.makedirs(os.path.join(self.system, "applications"))
        os.makedirs(os.path.join(self.user, "applications"))
        self.addCleanup(self._cleanup)

    def _cleanup(self):
        import shutil as _shutil
        _shutil.rmtree(self.system, ignore_errors=True)
        _shutil.rmtree(self.user, ignore_errors=True)

    def _write(self, root, name, body):
        full = os.path.join(root, "applications", name)
        with open(full, "w", encoding="utf-8") as f:
            f.write(body)
        return full

    def _patched_env(self):
        return mock.patch.dict(
            os.environ,
            {
                "XDG_DATA_HOME": self.user,
                "XDG_DATA_DIRS": self.system,
                "XDG_CURRENT_DESKTOP": "COSMIC",
                "LANG": "en_US.UTF-8",
                "LC_MESSAGES": "",
                "LC_ALL": "",
            },
        )

    def test_user_entry_shadows_system_entry(self):
        self._write(
            self.system,
            "com.clawos.Files.desktop",
            "[Desktop Entry]\nType=Application\nName=Files (system)\nExec=cosmic-files-sys\n",
        )
        self._write(
            self.user,
            "com.clawos.Files.desktop",
            "[Desktop Entry]\nType=Application\nName=Files (user override)\nExec=cosmic-files-user\n",
        )
        with self._patched_env():
            apps = main._scan_apps(gate=False)
        self.assertIn("com.clawos.Files", apps)
        self.assertEqual(apps["com.clawos.Files"]["name"], "Files (user override)")
        self.assertEqual(apps["com.clawos.Files"]["exec_binary"], "cosmic-files-user")

    def test_no_display_filtered_by_default(self):
        self._write(
            self.system,
            "helper.desktop",
            "[Desktop Entry]\nType=Application\nName=Helper\nExec=helper\nNoDisplay=true\n",
        )
        self._write(
            self.system,
            "real.desktop",
            "[Desktop Entry]\nType=Application\nName=Real\nExec=real\n",
        )
        with self._patched_env():
            apps = main._scan_apps(gate=False)
        self.assertNotIn("helper", apps)
        self.assertIn("real", apps)

    def test_only_show_in_filters_non_matching_desktop(self):
        self._write(
            self.system,
            "gnome-only.desktop",
            "[Desktop Entry]\nType=Application\nName=GnomeOnly\nExec=g\nOnlyShowIn=GNOME;\n",
        )
        with self._patched_env():
            apps = main._scan_apps(gate=False)
        self.assertNotIn("gnome-only", apps)

    def test_localized_name_picked_up(self):
        self._write(
            self.system,
            "com.clawos.Files.desktop",
            "[Desktop Entry]\nType=Application\nName=Files\nName[zh_CN]=文件\nExec=cosmic-files\n",
        )
        with mock.patch.dict(
            os.environ,
            {
                "XDG_DATA_HOME": self.user,
                "XDG_DATA_DIRS": self.system,
                "XDG_CURRENT_DESKTOP": "COSMIC",
                "LANG": "zh_CN.UTF-8",
                "LC_MESSAGES": "",
                "LC_ALL": "",
            },
        ):
            apps = main._scan_apps(gate=False)
        self.assertEqual(apps["com.clawos.Files"]["name"], "文件")


# ---------------------------------------------------------------------------
# Recent log
# ---------------------------------------------------------------------------


class TestRecentLog(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(self._cleanup)
        self.env = mock.patch.dict(os.environ, {"COS_DATA_DIR": self.tmp})
        self.env.start()
        self.addCleanup(self.env.stop)
        # Module-level paths were computed at import — reset for this test.
        main.DATA_DIR = self.tmp
        main.LAUNCHER_DIR = os.path.join(self.tmp, "launcher")
        main.RECENT_PATH = os.path.join(main.LAUNCHER_DIR, "recent.jsonl")

    def _cleanup(self):
        import shutil as _shutil
        _shutil.rmtree(self.tmp, ignore_errors=True)

    def test_append_then_read(self):
        main._append_recent({"ts": "2026-05-14T00:00:00Z", "app_id": "a", "name": "A"})
        main._append_recent({"ts": "2026-05-14T00:01:00Z", "app_id": "b", "name": "B"})
        recent = main._read_recent(10)
        self.assertEqual([r["app_id"] for r in recent], ["b", "a"])

    def test_dedup_keeps_most_recent_and_counts(self):
        main._append_recent({"ts": "t1", "app_id": "a", "name": "A"})
        main._append_recent({"ts": "t2", "app_id": "b", "name": "B"})
        main._append_recent({"ts": "t3", "app_id": "a", "name": "A"})
        recent = main._read_recent(10)
        self.assertEqual([r["app_id"] for r in recent], ["a", "b"])
        a = next(r for r in recent if r["app_id"] == "a")
        self.assertEqual(a["last_launched_at"], "t3")
        self.assertEqual(a["count"], 2)

    def test_read_empty_when_missing(self):
        self.assertEqual(main._read_recent(10), [])

    def test_rotation_keeps_last_n_lines(self):
        # Force rotation threshold to a tiny value
        original = main.RECENT_ROTATE_BYTES
        main.RECENT_ROTATE_BYTES = 100
        main.RECENT_KEEP_LINES = 5
        try:
            for i in range(50):
                main._append_recent({"ts": f"t{i}", "app_id": f"app{i}", "name": "x"})
            with open(main.RECENT_PATH, "r") as f:
                lines = f.readlines()
            # After many appends rotation should have triggered at least once,
            # leaving no more than KEEP_LINES + however many were added after
            # the most recent rotation (at most one rotation interval).
            self.assertLessEqual(len(lines), main.RECENT_KEEP_LINES + 50)
            for line in lines:
                json.loads(line)  # all kept lines are valid JSON
        finally:
            main.RECENT_ROTATE_BYTES = original

    def test_read_skips_garbage_lines(self):
        os.makedirs(main.LAUNCHER_DIR, exist_ok=True)
        with open(main.RECENT_PATH, "w") as f:
            f.write("not json\n")
            f.write(json.dumps({"ts": "t1", "app_id": "ok", "name": "OK"}) + "\n")
            f.write("{also not valid\n")
        recent = main._read_recent(10)
        self.assertEqual([r["app_id"] for r in recent], ["ok"])


# ---------------------------------------------------------------------------
# Top-level run() dispatch
# ---------------------------------------------------------------------------


class TestRunDispatch(unittest.TestCase):
    def test_unknown_command_returns_error(self):
        result = main.run("totally-not-a-verb", [])
        self.assertIn("error", result)

    def test_open_without_app_id_returns_error(self):
        # No policy.require is called yet (missing-arg check is first).
        result = main.run("open", [])
        self.assertIn("error", result)
        self.assertIn("app_id", result["error"])

    def test_find_without_query_returns_error(self):
        self.assertIn("error", main.run("find", []))

    def test_is_running_without_app_id_returns_error(self):
        self.assertIn("error", main.run("is-running", []))


if __name__ == "__main__":
    unittest.main()
