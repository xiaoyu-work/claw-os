import json
from pathlib import Path
from unittest import mock

from test_support import load_local_module


log_app = load_local_module(Path(__file__).with_name("main.py"), "log_app_main")


def test_write_preserves_delimited_option_shaped_message(tmp_path):
    log_app.LOG_DIR = str(tmp_path)
    log_app.LOG_FILE = str(tmp_path / "system.jsonl")
    with mock.patch.object(log_app.policy, "require"):
        result = log_app.run("write", ["--", "--level"])
    assert result["message"] == "--level"
    stored = json.loads((tmp_path / "system.jsonl").read_text(encoding="utf-8"))
    assert stored["message"] == "--level"
