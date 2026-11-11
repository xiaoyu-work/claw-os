import importlib.util
from pathlib import Path
from types import SimpleNamespace

import pytest


MODULE = Path(__file__).resolve().parents[2] / "tools/app_apt_metadata.py"
specification = importlib.util.spec_from_file_location("app_apt_metadata", MODULE)
metadata = importlib.util.module_from_spec(specification)
specification.loader.exec_module(metadata)


class Records:
    def __init__(self, digest):
        self.digest = digest
        self.current = None

    def lookup(self, entry):
        self.current = entry[0]
        return True

    @property
    def sha256_hash(self):
        if self.current == "status":
            raise SystemError("installed status records contain no archive hash")
        return self.digest


def source_fixture(uri, digest, *, trusted=True):
    version = SimpleNamespace(
        ver_str="1.0", arch="all", file_list=[("status", 0), ("index", 1)]
    )
    index = SimpleNamespace(is_trusted=trusted, archive_uri=lambda _: uri)
    sources = SimpleNamespace(find_index=lambda entry: index if entry == "index" else None)
    return {"fixture": SimpleNamespace(version_list=[version])}, Records(digest), sources


@pytest.mark.parametrize(
    ("uri", "app_source", "os_source"),
    [
        ("https://xiaoyu-work.github.io/clawos-app/", True, False),
        ("http://deb.debian.org/debian/", False, True),
    ],
)
def test_source_classification_binds_downloaded_bytes_and_skips_installed_status(
    uri, app_source, os_source
):
    package = {
        "package": "fixture", "version": "1.0", "architecture": "all", "sha256": "a" * 64
    }
    cache, records, sources = source_fixture(uri, package["sha256"])
    result = metadata.classify([package], cache, records, sources)
    assert result == [{**package, "app_source": app_source, "os_source": os_source}]


@pytest.mark.parametrize("trusted", [True, False])
def test_package_name_or_untrusted_index_cannot_authenticate_other_archive_bytes(trusted):
    package = {
        "package": "fixture", "version": "1.0", "architecture": "all", "sha256": "b" * 64
    }
    cache, records, sources = source_fixture(
        "https://xiaoyu-work.github.io/clawos-app/",
        "a" * 64 if trusted else package["sha256"],
        trusted=trusted,
    )
    with pytest.raises(ValueError, match="no matching authenticated APT index"):
        metadata.classify([package], cache, records, sources)


def test_foreign_os_dependency_architecture_still_requires_exact_index_bytes():
    package = {
        "package": "fixture", "version": "1.0", "architecture": "i386", "sha256": "c" * 64
    }
    cache, records, sources = source_fixture("http://deb.debian.org/debian/", package["sha256"])
    cache["fixture"].version_list[0].arch = "i386"
    cache["fixture:i386"] = cache.pop("fixture")
    assert metadata.classify([package], cache, records, sources) == [
        {**package, "app_source": False, "os_source": True}
    ]
    records.digest = "d" * 64
    with pytest.raises(ValueError, match="no matching authenticated APT index"):
        metadata.classify([package], cache, records, sources)
