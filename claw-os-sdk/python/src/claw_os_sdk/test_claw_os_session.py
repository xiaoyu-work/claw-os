"""Tests for the ``claw_os_session`` recording helpers — focus on
the concurrency / atomicity guarantees called out in the bug audit:
:func:`_atomic_write_json` must produce a file containing exactly one
full payload no matter how many threads race on it. JSONL sequence
allocation uses the log itself as the sole source of truth."""

import json
import os
import pathlib
import sys
import tempfile
import threading
import unittest

_THIS_DIR = os.path.dirname(__file__)
sys.path.insert(0, os.path.dirname(_THIS_DIR))

from claw_os_sdk import claw_os_session as cos  # noqa: E402


class AtomicWriteTests(unittest.TestCase):
    def test_concurrent_writers_leave_no_partial_file(self) -> None:
        """8 threads write distinct payloads to the same path 20 times
        each. After joining, the file must parse as JSON, and must
        match exactly one of the payloads we wrote — never a torn /
        merged blob. The tmp-suffix uniqueness fix is what makes this
        safe.
        """
        with tempfile.TemporaryDirectory() as td:
            path = pathlib.Path(td) / "concurrent.json"
            payloads = [
                {"who": f"thread-{i}", "data": [i] * 64}
                for i in range(8)
            ]
            errors: list[BaseException] = []
            barrier = threading.Barrier(len(payloads))

            def writer(payload: dict) -> None:
                try:
                    barrier.wait()
                    for _ in range(20):
                        cos._atomic_write_json(path, payload)
                except BaseException as e:  # noqa: BLE001
                    errors.append(e)

            threads = [
                threading.Thread(target=writer, args=(p,)) for p in payloads
            ]
            for t in threads:
                t.start()
            for t in threads:
                t.join()

            self.assertEqual(errors, [], f"writer thread(s) raised: {errors}")
            self.assertTrue(path.exists())
            with path.open("rb") as f:
                final = json.loads(f.read())
            # The final state must be exactly one of the payloads we
            # submitted — not a half-written one and not a merged
            # combination.
            self.assertIn(final, payloads)
            # And there must be no stale .tmp files left behind in the
            # directory once everyone joined.
            leftovers = [
                p for p in path.parent.iterdir() if p.name.startswith("concurrent.json.tmp.")
            ]
            self.assertEqual(leftovers, [], f"stash files left over: {leftovers}")


class StashBlobTests(unittest.TestCase):
    def _make_session(self, td: str) -> "cos.Session":
        os.environ["COS_DATA_DIR"] = td
        sid = "ses_0000000000000_000000000000"
        s = cos.Session(sid)
        s.dir.mkdir(parents=True, exist_ok=True)
        return s

    def test_distinct_calls_get_distinct_paths(self) -> None:
        """Every `_stash_blob` call allocates a fresh blob id, so even
        identical payloads land at different paths. The bug we fixed
        was that the *tmp* suffix used by the writer wasn't unique
        and could collide across threads — the blob ids themselves
        were already random per-call."""
        with tempfile.TemporaryDirectory() as td:
            try:
                s = self._make_session(td)
                p_a = s._stash_blob(b"hello world")
                p_b = s._stash_blob(b"hello world")
                self.assertNotEqual(p_a, p_b)
                inv = s.dir / "files" / "inverse"
                self.assertTrue((inv / f"{p_a}.bin").exists())
                self.assertTrue((inv / f"{p_b}.bin").exists())
                # And there should be no leftover .tmp files.
                leftovers = [
                    p for p in inv.iterdir() if ".tmp." in p.name
                ]
                self.assertEqual(leftovers, [], leftovers)
            finally:
                os.environ.pop("COS_DATA_DIR", None)

    def test_concurrent_stash_no_collision(self) -> None:
        """8 threads each stash a different payload 5 times. None of
        the per-thread tmp suffixes may collide and the final blob
        directory must contain 40 distinct .bin files."""
        with tempfile.TemporaryDirectory() as td:
            try:
                s = self._make_session(td)
                errors: list[BaseException] = []
                barrier = threading.Barrier(8)

                def worker(idx: int) -> None:
                    try:
                        barrier.wait()
                        for j in range(5):
                            s._stash_blob(f"t{idx}-{j}".encode())
                    except BaseException as e:  # noqa: BLE001
                        errors.append(e)

                ts = [threading.Thread(target=worker, args=(i,)) for i in range(8)]
                for t in ts:
                    t.start()
                for t in ts:
                    t.join()

                self.assertEqual(errors, [], errors)
                inv = s.dir / "files" / "inverse"
                bins = sorted(p.name for p in inv.iterdir() if p.suffix == ".bin")
                self.assertEqual(len(bins), 40, bins)
            finally:
                os.environ.pop("COS_DATA_DIR", None)


if __name__ == "__main__":
    unittest.main()
