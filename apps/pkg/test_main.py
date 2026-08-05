"""Tests for the pkg app — search & show parsing and arg handling."""

import json
import os
import pathlib
import sys
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(__file__))
sys.path.insert(
    0,
    os.path.join(
        os.path.dirname(__file__), os.pardir, os.pardir,
        "claw-os-sdk", "python", "src",
    ),
)  # for `from claw_os_sdk import …`
sys.path.insert(
    0,
    os.path.join(
        os.path.dirname(__file__), os.pardir, os.pardir,
        "cos-runtime", "python", "src",
    ),
)  # for `from cos_runtime import …`

from test_support import load_local_module

main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_pkg_main",
    clear_modules=("_shared",),
)
DEFAULT_SEARCH_LIMIT = main.DEFAULT_SEARCH_LIMIT
MAX_SEARCH_LIMIT = main.MAX_SEARCH_LIMIT
_apt_install = main._apt_install
_parse_apt_show = main._parse_apt_show
_parse_search_args = main._parse_search_args
_valid_package_name = main._valid_package_name
cmd_need = main.cmd_need
cmd_search = main.cmd_search
cmd_show = main.cmd_show
run = main.run


def _allow_policy():
    """Stub policy.require so tests don't shell out to the policy bridge."""
    return mock.patch.object(main.policy, "require", lambda *a, **kw: None)


def _fake_completed(stdout="", stderr="", returncode=0):
    return mock.Mock(stdout=stdout, stderr=stderr, returncode=returncode)


# ---------------------------------------------------------------------------
# _parse_search_args
# ---------------------------------------------------------------------------

class ParseSearchArgsTests(unittest.TestCase):
    def test_defaults_to_default_limit(self):
        query, limit = _parse_search_args(["image", "converter"])
        self.assertEqual(query, "image converter")
        self.assertEqual(limit, DEFAULT_SEARCH_LIMIT)

    def test_flag_limit_separate_value(self):
        query, limit = _parse_search_args(["pdf", "--limit", "5"])
        self.assertEqual(query, "pdf")
        self.assertEqual(limit, 5)

    def test_flag_limit_short(self):
        query, limit = _parse_search_args(["-n", "3", "json"])
        self.assertEqual(query, "json")
        self.assertEqual(limit, 3)

    def test_flag_limit_equals(self):
        query, limit = _parse_search_args(["--limit=7", "csv"])
        self.assertEqual(query, "csv")
        self.assertEqual(limit, 7)

    def test_limit_capped_at_max(self):
        _, limit = _parse_search_args(["foo", "--limit", str(MAX_SEARCH_LIMIT * 10)])
        self.assertEqual(limit, MAX_SEARCH_LIMIT)

    def test_non_positive_limit_rejected(self):
        with self.assertRaises(ValueError):
            _parse_search_args(["foo", "--limit", "0"])

    def test_non_integer_limit_rejected(self):
        with self.assertRaises(ValueError):
            _parse_search_args(["foo", "--limit", "many"])

    def test_missing_limit_value_rejected(self):
        with self.assertRaises(ValueError):
            _parse_search_args(["foo", "--limit"])


class PackageInstallTests(unittest.TestCase):
    def test_package_name_validation_rejects_options(self):
        self.assertTrue(_valid_package_name("python3-venv"))
        self.assertTrue(_valid_package_name("curl:amd64"))
        self.assertFalse(_valid_package_name("-oDpkg::Pre-Invoke::=id"))
        self.assertFalse(_valid_package_name("../curl"))

    def test_need_rejects_invalid_package_before_policy_check(self):
        with mock.patch.object(main.policy, "require") as require:
            result = cmd_need(["-oDpkg::Pre-Invoke::=id"])
        self.assertIn("error", result)
        require.assert_not_called()

    def test_install_uses_hidden_clawd_broker(self):
        payload = json.dumps({"package": "curl", "after": {"installed": True}})
        with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
            main.subprocess,
            "run",
            return_value=_fake_completed(stdout=payload),
        ) as runner:
            installed, failed, errors = _apt_install(["curl"])
        self.assertEqual(installed, ["curl"])
        self.assertEqual(failed, [])
        self.assertEqual(errors, {})
        argv = runner.call_args[0][0]
        self.assertEqual(argv, ["/usr/local/bin/cos", "__package", "install", "curl"])

    def test_remove_uses_exact_package_scope(self):
        payload = json.dumps({"action": "remove", "changed": True})
        with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
            main.policy, "require"
        ) as require, mock.patch.object(
            main.subprocess, "run", return_value=_fake_completed(stdout=payload)
        ):
            result = main.run("remove", ["curl"])
        require.assert_called_once_with("sys.package", name="curl")
        self.assertTrue(result["changed"])

    def test_update_requires_wild_package_permission(self):
        payload = json.dumps({"action": "update-index"})
        with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
            main.policy, "require"
        ) as require, mock.patch.object(
            main.subprocess, "run", return_value=_fake_completed(stdout=payload)
        ):
            main.run("update", [])
        require.assert_called_once_with("sys.package", wild=True)


# ---------------------------------------------------------------------------
# cmd_search
# ---------------------------------------------------------------------------

class CmdSearchTests(unittest.TestCase):
    def test_empty_args_returns_error(self):
        result = cmd_search([])
        self.assertIn("error", result)

    def test_only_flag_no_query_returns_error(self):
        with _allow_policy():
            result = cmd_search(["--limit", "5"])
        self.assertIn("error", result)

    def test_parses_results(self):
        fake_stdout = (
            "imagemagick - image manipulation programs -- binaries\n"
            "graphicsmagick - collection of image processing tools\n"
            "noseparator-line-without-dash\n"
        )
        with _allow_policy(), mock.patch.object(
            main.subprocess, "run", return_value=_fake_completed(stdout=fake_stdout)
        ):
            result = cmd_search(["image"])
        self.assertEqual(result["query"], "image")
        self.assertEqual(result["count"], 3)
        self.assertEqual(result["results"][0]["name"], "imagemagick")
        self.assertEqual(
            result["results"][0]["summary"],
            "image manipulation programs -- binaries",
        )
        self.assertEqual(result["results"][2]["name"], "noseparator-line-without-dash")
        self.assertEqual(result["results"][2]["summary"], "")

    def test_results_truncated_to_limit(self):
        lines = "\n".join(f"pkg{i} - summary {i}" for i in range(50))
        with _allow_policy(), mock.patch.object(
            main.subprocess, "run", return_value=_fake_completed(stdout=lines)
        ):
            result = cmd_search(["foo", "--limit", "10"])
        self.assertEqual(result["count"], 10)
        self.assertTrue(result["truncated"])
        self.assertIn("hint", result)

    def test_apt_cache_missing_returns_structured_error(self):
        with _allow_policy(), mock.patch.object(
            main.subprocess, "run", side_effect=FileNotFoundError
        ):
            result = cmd_search(["foo"])
        self.assertEqual(result["results"], [])
        self.assertIn("apt-cache not found", result["error"])

    def test_non_zero_exit_surfaces_stderr(self):
        with _allow_policy(), mock.patch.object(
            main.subprocess,
            "run",
            return_value=_fake_completed(stderr="E: oops", returncode=100),
        ):
            result = cmd_search(["foo"])
        self.assertEqual(result["results"], [])
        self.assertIn("oops", result["error"])

    def test_invokes_apt_cache_with_names_only(self):
        with _allow_policy(), mock.patch.object(
            main.subprocess, "run", return_value=_fake_completed(stdout="")
        ) as runner:
            cmd_search(["pdf", "viewer"])
        runner.assert_called_once()
        args = runner.call_args[0][0]
        self.assertEqual(args[:3], ["apt-cache", "search", "--names-only"])
        self.assertEqual(args[3], "pdf viewer")


# ---------------------------------------------------------------------------
# _parse_apt_show
# ---------------------------------------------------------------------------

class ParseAptShowTests(unittest.TestCase):
    def test_first_stanza_only(self):
        stdout = (
            "Package: foo\n"
            "Version: 1.0\n"
            "Description: short\n"
            " long line one\n"
            " long line two\n"
            "\n"
            "Package: foo\n"
            "Version: 0.9\n"
        )
        fields = _parse_apt_show(stdout)
        self.assertEqual(fields["Package"], "foo")
        self.assertEqual(fields["Version"], "1.0")
        self.assertIn("long line one", fields["Description"])
        self.assertIn("long line two", fields["Description"])

    def test_empty_input(self):
        self.assertEqual(_parse_apt_show(""), {})


# ---------------------------------------------------------------------------
# cmd_show
# ---------------------------------------------------------------------------

class CmdShowTests(unittest.TestCase):
    def test_empty_args(self):
        result = cmd_show([])
        self.assertIn("error", result)

    def test_returns_structured_metadata(self):
        stdout = (
            "Package: imagemagick\n"
            "Version: 8:6.9.11.60\n"
            "Section: graphics\n"
            "Homepage: https://imagemagick.org/\n"
            "Maintainer: Debian QA <pkg@debian.org>\n"
            "Installed-Size: 100\n"
            "Depends: libc6, libmagickcore\n"
            "Description: image manipulation programs -- binaries\n"
            " ImageMagick is a collection of tools for creating,\n"
            " editing, and converting images.\n"
        )
        with _allow_policy(), mock.patch.object(
            main.subprocess, "run", return_value=_fake_completed(stdout=stdout)
        ):
            result = cmd_show(["imagemagick"])
        self.assertTrue(result["found"])
        self.assertEqual(result["name"], "imagemagick")
        self.assertEqual(result["version"], "8:6.9.11.60")
        self.assertEqual(result["summary"], "image manipulation programs -- binaries")
        self.assertIn("ImageMagick is a collection", result["description"])
        self.assertEqual(result["homepage"], "https://imagemagick.org/")
        self.assertEqual(result["section"], "graphics")
        self.assertEqual(result["depends"], "libc6, libmagickcore")
        self.assertEqual(result["installed_size"], "100")

    def test_unknown_package_returns_not_found(self):
        with _allow_policy(), mock.patch.object(
            main.subprocess,
            "run",
            return_value=_fake_completed(stderr="N: Unable to locate package nope", returncode=100),
        ):
            result = cmd_show(["nope"])
        self.assertFalse(result["found"])
        self.assertIn("nope", result["error"])

    def test_apt_cache_missing(self):
        with _allow_policy(), mock.patch.object(
            main.subprocess, "run", side_effect=FileNotFoundError
        ):
            result = cmd_show(["foo"])
        self.assertFalse(result["found"])
        self.assertIn("apt-cache not found", result["error"])


# ---------------------------------------------------------------------------
# run() dispatcher
# ---------------------------------------------------------------------------

class RunDispatcherTests(unittest.TestCase):
    def test_schema_lists_new_commands(self):
        schema = run("__schema__", [])
        for cmd in ("need", "has", "list", "search", "show"):
            self.assertIn(cmd, schema, f"missing schema entry: {cmd}")

    def test_unknown_command(self):
        result = run("flarp", [])
        self.assertIn("error", result)

    def test_search_dispatch(self):
        with _allow_policy(), mock.patch.object(
            main.subprocess, "run", return_value=_fake_completed(stdout="abc - a thing\n")
        ):
            result = run("search", ["abc"])
        self.assertEqual(result["count"], 1)
        self.assertEqual(result["results"][0]["name"], "abc")

    def test_show_dispatch(self):
        stdout = "Package: abc\nVersion: 1\nDescription: thing\n"
        with _allow_policy(), mock.patch.object(
            main.subprocess, "run", return_value=_fake_completed(stdout=stdout)
        ):
            result = run("show", ["abc"])
        self.assertTrue(result["found"])
        self.assertEqual(result["name"], "abc")


if __name__ == "__main__":
    unittest.main()
