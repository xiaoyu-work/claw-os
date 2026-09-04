import base64
import importlib.util
import json
import os
import pathlib
import sys
import tempfile
import unittest
from unittest import mock


APP_DIR = pathlib.Path(__file__).resolve().parent
APPS_DIR = APP_DIR.parent
if str(APPS_DIR) not in sys.path:
    sys.path.insert(0, str(APPS_DIR))
if str(APP_DIR) not in sys.path:
    sys.path.insert(0, str(APP_DIR))

import main  # noqa: E402


def _load_native_host():
    spec = importlib.util.spec_from_file_location(
        "browser_attached_native_host",
        APP_DIR / "native_host.py",
    )
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ManifestContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.manifest = json.loads((APP_DIR / "app.json").read_text(encoding="utf-8"))

    def test_manifest_is_mcp_only(self) -> None:
        self.assertNotIn("operations", self.manifest)
        self.assertEqual(len(self.manifest["mcp"]["tools"]), 10)
        server = (APP_DIR / "server.py").read_text(encoding="utf-8")
        self.assertIn("App.from_manifest()", server)
        self.assertNotIn("serve_manifest_operations", server)
        self.assertNotIn("def run(", (APP_DIR / "main.py").read_text(encoding="utf-8"))

    def test_page_tools_derive_exact_host_scope(self) -> None:
        tools = {tool["name"]: tool for tool in self.manifest["mcp"]["tools"]}
        for name in (
            "browser-attached.nav.go",
            "browser-attached.dom.query",
            "browser-attached.dom.click",
            "browser-attached.dom.fill",
            "browser-attached.dom.fill_secret",
            "browser-attached.page.snapshot",
            "browser-attached.page.screenshot",
            "browser-attached.eval",
        ):
            browser_need = next(
                need for need in tools[name]["needs"] if need["verb"].startswith("browser.")
            )
            self.assertEqual(browser_need["scope"]["kind"], "from-arg")
            self.assertEqual(browser_need["scope"]["transform"], "url-host")
        self.assertEqual(
            next(
                need
                for need in tools["browser-attached.dom.query"]["needs"]
                if need["verb"] == "browser.dom.read"
            )["scope"]["arg"],
            "page_url",
        )

    def test_tab_ids_are_typed_integers(self) -> None:
        for tool in self.manifest["mcp"]["tools"]:
            tab_id = next(
                (arg for arg in tool.get("args", []) if arg["name"] == "tab_id"),
                None,
            )
            if tab_id is not None:
                self.assertEqual(tab_id["kind"], "integer")


class BrowserOperationTests(unittest.TestCase):
    @mock.patch("main.browser_bridge.request")
    @mock.patch("main.policy.require", autospec=True)
    def test_tab_listing_uses_the_declared_wild_scope(
        self,
        require: mock.Mock,
        request: mock.Mock,
    ) -> None:
        request.return_value = {
            "tabs": [
                {
                    "id": 1,
                    "title": "Example",
                    "url": "https://example.com/",
                    "active": True,
                }
            ]
        }
        self.assertEqual(main.tabs_list()["tabs"][0]["id"], 1)
        require.assert_called_once_with("browser.tabs.read", wild=True)

    @mock.patch("main.browser_bridge.request")
    @mock.patch("main.policy.require", autospec=True)
    def test_page_action_uses_exact_default_port_scope(
        self,
        require: mock.Mock,
        request: mock.Mock,
    ) -> None:
        request.return_value = {"matches": [], "total": 0, "truncated": False}
        result = main.dom_query(9, "main", "https://Example.COM/path")
        self.assertEqual(result["total"], 0)
        require.assert_called_once_with("browser.dom.read", host="example.com:443")
        request.assert_called_once_with(
            "dom.query",
            tab_id=9,
            page_url="https://Example.COM/path",
            selector="main",
        )

    @mock.patch("main.memory.remember")
    @mock.patch("main.browser_bridge.request")
    @mock.patch("main.policy.require", autospec=True)
    def test_navigation_memory_failure_propagates(
        self,
        require: mock.Mock,
        request: mock.Mock,
        remember: mock.Mock,
    ) -> None:
        request.return_value = {
            "navigated": 4,
            "url": "https://example.com/login?token=secret",
        }
        remember.side_effect = RuntimeError("memory unavailable")
        with self.assertRaisesRegex(RuntimeError, "memory unavailable"):
            main.navigate(4, "https://example.com/login?token=secret")
        require.assert_called_once_with("browser.nav", host="example.com:443")
        self.assertEqual(
            remember.call_args.kwargs["text"],
            "Navigated browser tab 4 to host example.com:443",
        )

    @mock.patch("main.atomic_create_bytes")
    @mock.patch("main.browser_bridge.request")
    @mock.patch("main.policy.require", autospec=True)
    def test_screenshot_is_validated_and_written_atomically(
        self,
        require: mock.Mock,
        request: mock.Mock,
        atomic_write: mock.Mock,
    ) -> None:
        image = b"\x89PNG\r\n\x1a\ncontents"
        request.return_value = {"data": base64.b64encode(image).decode()}
        result = main.page_screenshot(
            3,
            "capture.png",
            "https://example.com/dashboard",
        )
        self.assertEqual(result["bytes"], len(image))
        self.assertEqual(require.call_args_list[0].args, ("browser.dom.read",))
        self.assertEqual(
            require.call_args_list[0].kwargs,
            {"host": "example.com:443"},
        )
        atomic_write.assert_called_once_with(
            result["saved"],
            image,
            mode=0o600,
        )

    @mock.patch("main.atomic_create_bytes")
    @mock.patch("main.browser_bridge.request", return_value={"data": "not base64!"})
    @mock.patch("main.policy.require", autospec=True)
    def test_invalid_screenshot_is_not_written(
        self,
        _require: mock.Mock,
        _request: mock.Mock,
        atomic_write: mock.Mock,
    ) -> None:
        with self.assertRaisesRegex(RuntimeError, "invalid base64"):
            main.page_screenshot(3, "capture.png", "https://example.com/")
        atomic_write.assert_not_called()

    @mock.patch("main.browser_bridge.request")
    @mock.patch("main.policy.require", autospec=True)
    def test_screenshot_refuses_to_replace_an_existing_path(
        self,
        require: mock.Mock,
        request: mock.Mock,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory) / "capture.png"
            output.write_bytes(b"existing")
            with self.assertRaisesRegex(ValueError, "new path"):
                main.page_screenshot(3, str(output), "https://example.com/")
        require.assert_not_called()
        request.assert_not_called()

    @mock.patch("main.browser_bridge.request")
    @mock.patch("main.policy.require", autospec=True)
    def test_screenshot_refuses_a_dangling_output_symlink(
        self,
        require: mock.Mock,
        request: mock.Mock,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory) / "capture.png"
            output.symlink_to(pathlib.Path(directory) / "missing.png")
            with self.assertRaisesRegex(ValueError, "new path"):
                main.page_screenshot(3, str(output), "https://example.com/")
        require.assert_not_called()
        request.assert_not_called()

    def test_atomic_screenshot_create_never_replaces_a_racing_file(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory) / "capture.png"
            main.atomic_create_bytes(str(output), b"first", mode=0o600)
            self.assertEqual(os.stat(output).st_mode & 0o777, 0o600)
            with self.assertRaises(FileExistsError):
                main.atomic_create_bytes(str(output), b"second", mode=0o600)
            self.assertEqual(output.read_bytes(), b"first")


class NativeHostTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.host = _load_native_host()

    def test_socket_override_is_not_accepted(self) -> None:
        with mock.patch.dict(
            "os.environ",
            {
                "XDG_RUNTIME_DIR": "/tmp/not-the-runtime-dir",
                "CLAW_BROWSER_SOCK": "/tmp/attacker.sock",
            },
            clear=True,
        ):
            with self.assertRaisesRegex(RuntimeError, "must resolve"):
                self.host._socket_path()

    def test_request_schema_is_closed(self) -> None:
        self.host._validate_request({"id": "1", "verb": "tabs.list", "args": {}})
        with self.assertRaisesRegex(ValueError, "unexpected fields"):
            self.host._validate_request(
                {
                    "id": "1",
                    "verb": "tabs.list",
                    "args": {},
                    "allow_eval": True,
                }
            )

    def test_peer_uid_comes_from_kernel_credentials(self) -> None:
        conn = mock.Mock()
        conn.getsockopt.return_value = __import__("struct").pack("3i", 123, 0, 456)
        self.assertEqual(self.host._peer_uid(conn), 0)


if __name__ == "__main__":
    unittest.main()
