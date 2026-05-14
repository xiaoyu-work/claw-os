"""Regression tests for `_lib.snapshot._allocate_seq_dir`.

Pre-fix `_take_snapshot` chose the next sequence directory via a
TOCTOU pattern: scan `listdir`, take `max + 1`, then `os.makedirs(
exist_ok=True)`. Two concurrent writers in the same trash dir
could both compute the same seq and both succeed, silently
stomping each other's `blob` / `meta.json`. The fix replaces the
TOCTOU with `os.mkdir` + retry on `FileExistsError`.
"""

import os
import sys
import tempfile
import threading
import unittest

_THIS_DIR = os.path.dirname(__file__)
sys.path.insert(0, os.path.dirname(_THIS_DIR))  # so `from _lib import snapshot` works

from _lib import snapshot  # noqa: E402


class AllocateSeqDirTests(unittest.TestCase):
    def test_first_allocation_is_000001(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            sid_dir = os.path.join(tmp, "ses_x")
            seq, entry = snapshot._allocate_seq_dir(sid_dir)
            self.assertEqual(seq, "000001")
            self.assertTrue(os.path.isdir(entry))
            self.assertEqual(os.path.basename(entry), "000001")

    def test_sequential_allocations_increment(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            sid_dir = os.path.join(tmp, "ses_x")
            seqs = [snapshot._allocate_seq_dir(sid_dir)[0] for _ in range(5)]
            self.assertEqual(seqs, ["000001", "000002", "000003", "000004", "000005"])

    def test_skips_over_preexisting_seq_dir(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            sid_dir = os.path.join(tmp, "ses_x")
            os.makedirs(os.path.join(sid_dir, "000005"))
            seq, _ = snapshot._allocate_seq_dir(sid_dir)
            # Allocator must not return an already-existing seq.
            self.assertNotEqual(seq, "000005")
            # And must pick something strictly greater than the
            # highest existing entry.
            self.assertGreater(int(seq), 5)

    def test_concurrent_allocations_yield_distinct_dirs(self) -> None:
        """The race the fix targets: 8 threads each allocate 25
        snapshots in the same sid_dir. Each allocation must land in
        its own directory — no two callers may receive the same
        seq, and the total number of directories must equal the
        total number of allocations.
        """
        with tempfile.TemporaryDirectory() as tmp:
            sid_dir = os.path.join(tmp, "ses_concurrent")
            per_thread = 25
            n_threads = 8
            results: list[str] = []
            results_lock = threading.Lock()

            def worker() -> None:
                local: list[str] = []
                for _ in range(per_thread):
                    seq, _entry = snapshot._allocate_seq_dir(sid_dir)
                    local.append(seq)
                with results_lock:
                    results.extend(local)

            threads = [threading.Thread(target=worker) for _ in range(n_threads)]
            for t in threads:
                t.start()
            for t in threads:
                t.join()

            self.assertEqual(len(results), n_threads * per_thread)
            self.assertEqual(
                len(set(results)),
                n_threads * per_thread,
                "concurrent _allocate_seq_dir produced duplicate seq values",
            )
            on_disk = sorted(
                name for name in os.listdir(sid_dir) if name.isdigit()
            )
            self.assertEqual(
                len(on_disk),
                n_threads * per_thread,
                "fewer dirs on disk than allocations — writers stomped each other",
            )


if __name__ == "__main__":
    unittest.main()
