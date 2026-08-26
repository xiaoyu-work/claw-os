"""Tests for the bundled App credential loader."""

import json
import pathlib
import sys
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from _shared import credentials


class CredentialLoaderTests(unittest.TestCase):
    @mock.patch.object(credentials.subprocess, "run")
    def test_loads_value_from_cos_json(self, run):
        run.return_value = mock.MagicMock(
            returncode=0,
            stdout=json.dumps({"value": "secret-value"}),
            stderr="",
        )

        value, error = credentials.load_credential("TOKEN")

        self.assertEqual(value, "secret-value")
        self.assertIsNone(error)

    @mock.patch.object(credentials.subprocess, "run")
    def test_suppresses_child_stderr_on_failure(self, run):
        run.return_value = mock.MagicMock(
            returncode=1,
            stdout="",
            stderr="must-not-leak-secret-value",
        )

        value, error = credentials.load_credential("TOKEN")

        self.assertIsNone(value)
        self.assertNotIn("must-not-leak", error)
        self.assertIn("stderr suppressed", error)


if __name__ == "__main__":
    unittest.main()
