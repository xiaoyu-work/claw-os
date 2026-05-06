//! Local archive install — Phase 2.1.
//!
//! Lays a `.zip` (or eventually `.tar.gz`) archive down at
//! `<engines_dir>/<engine>/<version>/`, then registers the install in
//! `engines.json`. **No network, no version inference**: caller passes
//! `engine` + `version` + path to the archive.
//!
//! ## Atomicity
//!
//! 1. Extract under `<engines_dir>/<engine>/.staging-<rand>/`
//! 2. Validate that the extracted tree contains at least one usable
//!    sub-directory (`bin/` or `lib/`).
//! 3. Atomic-rename the staging dir to `<engine>/<version>/`. Fails if
//!    the destination already exists.
//! 4. Register in `engines.json` and `save()`.
//!
//! On any error we roll back by removing the staging dir.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use chrono::Utc;
use uuid::Uuid;

use super::registry::{EnginesIndex, InstalledVersion, RegistryError};

#[derive(Debug)]
pub struct InstallResult {
    pub install_dir: PathBuf,
    pub files_extracted: usize,
    pub bytes_on_disk: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("registry: {0}")]
    Registry(#[from] RegistryError),
    #[error("archive does not exist: {0}")]
    ArchiveMissing(PathBuf),
    #[error(
        "unsupported archive format: {0} (Phase 2.1 supports .zip; tar.gz lands with P2.2 GitHub adapter)"
    )]
    UnsupportedFormat(String),
    #[error("archive is empty after extraction")]
    EmptyArchive,
    #[error("archive layout invalid: missing bin/ or lib/ directory under {0}")]
    InvalidLayout(PathBuf),
    #[error("destination already exists: {0} (uninstall first or use a different version)")]
    DestinationExists(PathBuf),
    #[error("zip slip detected — entry path escapes destination: {0}")]
    PathTraversal(String),
}

pub fn install_from_archive(
    index: &mut EnginesIndex,
    engine: &str,
    version: &str,
    archive: &Path,
) -> Result<InstallResult, InstallError> {
    if !archive.exists() {
        return Err(InstallError::ArchiveMissing(archive.to_path_buf()));
    }
    let install_dir = super::paths::engine_version_dir(engine, version);
    if install_dir.exists() {
        return Err(InstallError::DestinationExists(install_dir));
    }

    let engine_dir = super::paths::engine_dir(engine);
    fs::create_dir_all(&engine_dir)?;

    let staging = engine_dir.join(format!(".staging-{}", Uuid::new_v4()));
    fs::create_dir_all(&staging)?;

    let extract_result = extract(archive, &staging);
    let extract = match extract_result {
        Ok(e) => e,
        Err(e) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(e);
        }
    };

    if extract.files == 0 {
        let _ = fs::remove_dir_all(&staging);
        return Err(InstallError::EmptyArchive);
    }

    let canonical = match strip_single_wrapper(&staging)? {
        Some(inner) => inner,
        None => staging.clone(),
    };

    if !is_usable_engine_layout(&canonical) {
        let _ = fs::remove_dir_all(&staging);
        return Err(InstallError::InvalidLayout(canonical));
    }

    if canonical != staging {
        fs::rename(&canonical, &install_dir)?;
        let _ = fs::remove_dir_all(&staging);
    } else {
        fs::rename(&staging, &install_dir)?;
    }

    let bytes = dir_size(&install_dir).unwrap_or(0);
    index.record_install(
        engine,
        InstalledVersion {
            version: version.to_string(),
            installed_at: Utc::now(),
            bytes,
            source: format!("local:{}", archive.display()),
            sha256: String::new(),
        },
    )?;
    if index.entry_mut(engine).source.is_empty() {
        index.entry_mut(engine).source = "local".to_string();
    }
    index.save()?;
    Ok(InstallResult {
        install_dir,
        files_extracted: extract.files,
        bytes_on_disk: bytes,
    })
}

struct ExtractStats {
    files: usize,
}

fn extract(archive: &Path, dest: &Path) -> Result<ExtractStats, InstallError> {
    let ext = archive
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "zip" => extract_zip(archive, dest),
        other => Err(InstallError::UnsupportedFormat(other.to_string())),
    }
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<ExtractStats, InstallError> {
    let file = File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut files = 0;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let raw_name = match entry.enclosed_name() {
            Some(p) => p.to_path_buf(),
            None => {
                return Err(InstallError::PathTraversal(entry.name().to_string()));
            }
        };
        let outpath = dest.join(&raw_name);
        if !outpath.starts_with(dest) {
            return Err(InstallError::PathTraversal(entry.name().to_string()));
        }
        if entry.is_dir() {
            fs::create_dir_all(&outpath)?;
        } else {
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = File::create(&outpath)?;
            io::copy(&mut entry, &mut out)?;
            files += 1;
        }
    }
    Ok(ExtractStats { files })
}

fn strip_single_wrapper(dir: &Path) -> Result<Option<PathBuf>, InstallError> {
    let entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    if entries.len() != 1 {
        return Ok(None);
    }
    let only = &entries[0];
    if only.file_type()?.is_dir() {
        Ok(Some(only.path()))
    } else {
        Ok(None)
    }
}

fn is_usable_engine_layout(root: &Path) -> bool {
    root.join("bin").is_dir() || root.join("lib").is_dir()
}

fn dir_size(p: &Path) -> io::Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(p)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        if ft.is_dir() {
            total = total.saturating_add(dir_size(&entry.path())?);
        } else if ft.is_file() {
            total = total.saturating_add(entry.metadata()?.len());
        }
    }
    Ok(total)
}

#[allow(dead_code)]
pub fn sha256_of(path: &Path) -> io::Result<String> {
    use std::io::Read as _;
    let mut f = File::open(path)?;
    let mut hasher = Sha256Stream::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize_hex())
}

// In-tree SHA-256 (FIPS 180-4) so we don't pull a new crate just for
// archive verification.
#[allow(dead_code)]
struct Sha256Stream {
    state: [u32; 8],
    buffer: Vec<u8>,
    total_bits: u64,
}

#[allow(dead_code)]
impl Sha256Stream {
    fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c,
                0x1f83d9ab, 0x5be0cd19,
            ],
            buffer: Vec::with_capacity(64),
            total_bits: 0,
        }
    }

    fn update(&mut self, data: &[u8]) {
        self.total_bits = self.total_bits.wrapping_add((data.len() as u64) * 8);
        self.buffer.extend_from_slice(data);
        while self.buffer.len() >= 64 {
            let block: [u8; 64] = self.buffer[..64].try_into().unwrap();
            self.compress(&block);
            self.buffer.drain(..64);
        }
    }

    fn finalize_hex(mut self) -> String {
        let bits = self.total_bits;
        self.buffer.push(0x80);
        while self.buffer.len() % 64 != 56 {
            self.buffer.push(0);
        }
        self.buffer.extend_from_slice(&bits.to_be_bytes());
        let mut i = 0;
        while i < self.buffer.len() {
            let block: [u8; 64] = self.buffer[i..i + 64].try_into().unwrap();
            self.compress(&block);
            i += 64;
        }
        let mut out = String::with_capacity(64);
        for w in self.state {
            out.push_str(&format!("{:08x}", w));
        }
        out
    }

    fn compress(&mut self, block: &[u8; 64]) {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 =
                w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let mj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(mj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }
}

#[cfg(test)]
mod tests {
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
        zw.start_file(format!("{prefix}bin/llama-cli.exe"), opts).unwrap();
        zw.write_all(b"MZ\x00\x00fake exe").unwrap();
        zw.start_file(format!("{prefix}lib/llama.dll"), opts).unwrap();
        zw.write_all(b"MZ\x00\x00fake dll").unwrap();
        zw.start_file(format!("{prefix}include/llama.h"), opts).unwrap();
        zw.write_all(b"// header").unwrap();
        zw.finish().unwrap();
        f
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
            .suffix(".tar.gz")
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
}
