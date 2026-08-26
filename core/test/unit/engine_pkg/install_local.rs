use super::*;
use std::io::Write;

struct EnginesDirGuard {
    _td: tempfile::TempDir,
}

impl EnginesDirGuard {
    fn new() -> Self {
        let td = tempfile::Builder::new()
            .prefix("cos-engines-install-")
            .tempdir()
            .unwrap();
        super::super::paths::set_engines_dir_override(Some(td.path().to_path_buf()));
        Self { _td: td }
    }
}

impl Drop for EnginesDirGuard {
    fn drop(&mut self) {
        super::super::paths::set_engines_dir_override(None);
    }
}

fn make_zip(wrap: bool) -> tempfile::NamedTempFile {
    let f = tempfile::Builder::new()
        .prefix("cos-engine-archive-")
        .suffix(".zip")
        .tempfile()
        .unwrap();
    let mut zw = zip::ZipWriter::new(f.reopen().unwrap());
    let opts: zip::write::FileOptions<()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let prefix = if wrap { "wrapper/" } else { "" };
    zw.start_file(format!("{prefix}bin/llama-cli.exe"), opts)
        .unwrap();
    zw.write_all(b"MZ\x00\x00fake exe").unwrap();
    zw.start_file(format!("{prefix}lib/llama.dll"), opts)
        .unwrap();
    zw.write_all(b"MZ\x00\x00fake dll").unwrap();
    zw.start_file(format!("{prefix}include/llama.h"), opts)
        .unwrap();
    zw.write_all(b"// header").unwrap();
    zw.finish().unwrap();
    f
}

fn make_tar_gz(wrap: bool) -> tempfile::NamedTempFile {
    let f = tempfile::Builder::new()
        .prefix("cos-engine-archive-")
        .suffix(".tar.gz")
        .tempfile()
        .unwrap();
    let enc =
        flate2::write::GzEncoder::new(f.reopen().unwrap(), flate2::Compression::default());
    let mut tar = tar::Builder::new(enc);
    let prefix = if wrap { "wrapper/" } else { "" };
    append_tar_file(
        &mut tar,
        &format!("{prefix}bin/llama-cli"),
        b"fake exe",
        0o755,
    );
    append_tar_file(
        &mut tar,
        &format!("{prefix}lib/libllama.so"),
        b"fake so",
        0o644,
    );
    append_tar_file(
        &mut tar,
        &format!("{prefix}include/llama.h"),
        b"// header",
        0o644,
    );
    tar.finish().unwrap();
    let enc = tar.into_inner().unwrap();
    enc.finish().unwrap();
    f
}

fn append_tar_file<W: Write>(tar: &mut tar::Builder<W>, path: &str, body: &[u8], mode: u32) {
    let mut header = tar::Header::new_gnu();
    header.set_size(body.len() as u64);
    header.set_mode(mode);
    header.set_cksum();
    tar.append_data(&mut header, path, body).unwrap();
}

fn make_bad_zip() -> tempfile::NamedTempFile {
    let f = tempfile::Builder::new()
        .prefix("cos-engine-bad-")
        .suffix(".zip")
        .tempfile()
        .unwrap();
    let mut zw = zip::ZipWriter::new(f.reopen().unwrap());
    let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default();
    zw.start_file("README.md", opts).unwrap();
    zw.write_all(b"hi").unwrap();
    zw.finish().unwrap();
    f
}

#[test]
fn install_from_zip_succeeds_and_records() {
    let _g = EnginesDirGuard::new();
    let zip = make_zip(false);
    let mut idx = EnginesIndex::empty();
    let result = install_from_archive(&mut idx, "llama-cpp", "b4001", zip.path()).unwrap();
    assert_eq!(result.files_extracted, 3);
    assert!(result.install_dir.join("bin/llama-cli.exe").exists());
    assert!(result.install_dir.join("lib/llama.dll").exists());
    let entry = idx.entry("llama-cpp").unwrap();
    assert_eq!(entry.installed.len(), 1);
    assert_eq!(entry.installed[0].version, "b4001");
    assert!(entry.installed[0].bytes > 0);
}

#[test]
fn install_from_tar_gz_succeeds_and_records() {
    let _g = EnginesDirGuard::new();
    let archive = make_tar_gz(false);
    let mut idx = EnginesIndex::empty();
    let result = install_from_archive(&mut idx, "llama-cpp", "b4001", archive.path()).unwrap();
    assert_eq!(result.files_extracted, 3);
    assert!(result.install_dir.join("bin/llama-cli").exists());
    assert!(result.install_dir.join("lib/libllama.so").exists());
    let entry = idx.entry("llama-cpp").unwrap();
    assert_eq!(entry.installed.len(), 1);
    assert_eq!(entry.installed[0].version, "b4001");
}

#[test]
fn install_strips_single_wrapper_directory() {
    let _g = EnginesDirGuard::new();
    let zip = make_zip(true);
    let mut idx = EnginesIndex::empty();
    let result = install_from_archive(&mut idx, "llama-cpp", "b4001", zip.path()).unwrap();
    assert!(result.install_dir.join("bin/llama-cli.exe").exists());
    assert!(!result.install_dir.join("wrapper").exists());
}

#[test]
fn install_rejects_missing_archive() {
    let _g = EnginesDirGuard::new();
    let mut idx = EnginesIndex::empty();
    let err = install_from_archive(
        &mut idx,
        "llama-cpp",
        "b4001",
        Path::new("/this/does/not/exist.zip"),
    )
    .unwrap_err();
    assert!(matches!(err, InstallError::ArchiveMissing(_)));
}

#[test]
fn install_rejects_bad_layout() {
    let _g = EnginesDirGuard::new();
    let bad = make_bad_zip();
    let mut idx = EnginesIndex::empty();
    let err = install_from_archive(&mut idx, "llama-cpp", "b4001", bad.path()).unwrap_err();
    assert!(matches!(err, InstallError::InvalidLayout(_)));
    assert!(!super::super::paths::engine_version_dir("llama-cpp", "b4001").exists());
}

#[test]
fn install_rejects_unsupported_format() {
    let _g = EnginesDirGuard::new();
    let f = tempfile::Builder::new()
        .prefix("cos-engine-unsup-")
        .suffix(".txt")
        .tempfile()
        .unwrap();
    let mut idx = EnginesIndex::empty();
    let err = install_from_archive(&mut idx, "llama-cpp", "b4001", f.path()).unwrap_err();
    assert!(matches!(err, InstallError::UnsupportedFormat(_)));
}

#[test]
fn install_rejects_existing_destination() {
    let _g = EnginesDirGuard::new();
    let zip = make_zip(false);
    let mut idx = EnginesIndex::empty();
    install_from_archive(&mut idx, "llama-cpp", "b4001", zip.path()).unwrap();
    let err = install_from_archive(&mut idx, "llama-cpp", "b4001", zip.path()).unwrap_err();
    assert!(matches!(err, InstallError::DestinationExists(_)));
}

#[test]
fn sha256_matches_known_vector() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(b"abc").unwrap();
    let h = sha256_of(f.path()).unwrap();
    assert_eq!(
        h,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn sha256_empty_string() {
    let f = tempfile::NamedTempFile::new().unwrap();
    let h = sha256_of(f.path()).unwrap();
    assert_eq!(
        h,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}
