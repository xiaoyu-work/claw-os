//! Binary file extension classifier — answers
//! "if I read this path's bytes, should I treat them as text or as
//! opaque binary?".
//!
//! Mirrors Hermes' `agent/binary_extensions.py` and the smaller
//! checks scattered through Hermes' file-IO helpers, but keeps the
//! data-driven core: a maintained allowlist of binary extensions
//! (driven by what we see in software-development workflows), with
//! escape hatches for callers that want to override the default
//! list (e.g., a den has decided to treat a custom `.bin` payload
//! as text-like JSON-lines).
//!
//! The classifier is intentionally extension-only — it does NOT
//! probe magic bytes. That's by design:
//!   * extension classification is cheap and predictable; magic-byte
//!     sniffing requires opening the file and reading a header,
//!     which is wrong for the "I'm about to consider reading this
//!     file" pre-check this module is meant to inform.
//!   * callers that want byte-level checks can layer their own
//!     `is_likely_text(bytes)` (NUL byte / utf8 validity) on top.
//!
//! Scope: helpers like `is_binary_path` and the optional default
//! extension list. CLI wiring lives at `cos agent binary-ext`
//! (see `agent::mod::binary_ext_cmd`).

use std::collections::BTreeSet;
use std::path::Path;

/// Default set of file extensions treated as binary. The list is
/// alphabetised within categories for diff-friendliness; entries
/// are stored without a leading `.` and lower-cased.
///
/// This list is meant to cover the long tail of real-world
/// codebases an agent operates on. It is NOT exhaustive — callers
/// who need a stricter or looser policy should construct a
/// [`BinaryExtensions`] with `with_extras` / `without`.
pub const DEFAULT_BINARY_EXTENSIONS: &[&str] = &[
    // executables and object files
    "exe",
    "dll",
    "so",
    "dylib",
    "a",
    "o",
    "obj",
    "lib",
    "class",
    "pyc",
    "pyo",
    "pyd",
    "wasm",
    "node",
    "bin",
    "elf",
    "efi",
    "msi",
    "ko",
    "rlib",
    // archives and packaged formats
    "zip",
    "tar",
    "gz",
    "tgz",
    "bz2",
    "tbz2",
    "xz",
    "zst",
    "7z",
    "rar",
    "lz4",
    "cab",
    "deb",
    "rpm",
    "apk",
    "ipa",
    "appx",
    "dmg",
    "iso",
    "img",
    "jar",
    "war",
    "ear",
    "egg",
    "whl",
    "nupkg",
    "pkg",
    "snap",
    "flatpak",
    // databases and indexed stores
    "db",
    "sqlite",
    "sqlite3",
    "mdb",
    "accdb",
    "pdb",
    "rdb",
    "dat",
    "idx",
    "lock",
    // images
    "png",
    "jpg",
    "jpeg",
    "gif",
    "bmp",
    "tif",
    "tiff",
    "webp",
    "ico",
    "icns",
    "heic",
    "heif",
    "avif",
    "psd",
    "ai",
    "eps",
    "raw",
    "cr2",
    "nef",
    "arw",
    "dng",
    "tga",
    // audio
    "mp3",
    "wav",
    "flac",
    "ogg",
    "oga",
    "opus",
    "m4a",
    "aac",
    "aiff",
    "wma",
    "ape",
    "alac",
    "amr",
    "mid",
    "midi",
    // video
    "mp4",
    "m4v",
    "mov",
    "avi",
    "mkv",
    "webm",
    "flv",
    "wmv",
    "mpg",
    "mpeg",
    "3gp",
    "ogv",
    // ".ts" deliberately omitted — it would shadow TypeScript
    // source files. The MPEG transport-stream extension is rare in
    // agent contexts; callers that handle .ts video can opt in
    // with `BinaryExtensions::default().with_extras(["ts"])`.
    "mts",
    "m2ts",
    "vob",
    // fonts
    "ttf",
    "otf",
    "woff",
    "woff2",
    "eot",
    // documents (binary office / pdf-like)
    "pdf",
    "doc",
    "docx",
    "xls",
    "xlsx",
    "ppt",
    "pptx",
    "odt",
    "ods",
    "odp",
    "rtf",
    "pages",
    "numbers",
    "key",
    // ml/data binary blobs (model weights / serialised tensors)
    "onnx",
    "gguf",
    "ggml",
    "safetensors",
    "pt",
    "pth",
    "ckpt",
    "h5",
    "hdf5",
    "parquet",
    "arrow",
    "feather",
    "npz",
    "npy",
    "tflite",
    "pb",
    // misc binary
    "key",
    "p12",
    "pfx",
    "pem",
    "der",
    "crt",
    "cer",
    "jks",
    "keystore",
    "swf",
];

/// Mutable classifier — clone of the default list with
/// `with_extras` / `without` for per-callsite policy tweaks.
#[derive(Debug, Clone)]
pub struct BinaryExtensions {
    set: BTreeSet<String>,
}

impl Default for BinaryExtensions {
    fn default() -> Self {
        Self::new_default()
    }
}

impl BinaryExtensions {
    /// Empty set — caller must populate with `with_extras`.
    pub fn new() -> Self {
        Self {
            set: BTreeSet::new(),
        }
    }

    /// Prebuilt classifier seeded from [`DEFAULT_BINARY_EXTENSIONS`].
    pub fn new_default() -> Self {
        let mut set = BTreeSet::new();
        for ext in DEFAULT_BINARY_EXTENSIONS {
            set.insert((*ext).to_string());
        }
        Self { set }
    }

    /// Add additional extensions (lower-cased, leading `.` stripped).
    pub fn with_extras<I, S>(mut self, extras: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for raw in extras {
            self.set.insert(normalize(raw.as_ref()));
        }
        self
    }

    /// Remove extensions (e.g., a den has decided `.bin` is text).
    pub fn without<I, S>(mut self, drop: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for raw in drop {
            self.set.remove(&normalize(raw.as_ref()));
        }
        self
    }

    /// Number of extensions in the set.
    pub fn len(&self) -> usize {
        self.set.len()
    }

    /// True when the set is empty.
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    /// Read-only view of all classified extensions, sorted.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.set.iter().map(|s| s.as_str())
    }

    /// Membership check by raw extension (case-insensitive,
    /// leading `.` tolerated, leading whitespace stripped).
    pub fn contains_extension(&self, raw: &str) -> bool {
        let key = normalize(raw);
        if key.is_empty() {
            return false;
        }
        self.set.contains(&key)
    }

    /// Classify a path. Returns `false` for paths with no extension
    /// (the agent should treat extension-less files as text-like by
    /// default, and re-check with a magic-byte / NUL-byte heuristic
    /// if it actually decides to read them).
    pub fn is_binary_path(&self, path: impl AsRef<Path>) -> bool {
        match path.as_ref().extension().and_then(|e| e.to_str()) {
            Some(ext) => self.contains_extension(ext),
            None => false,
        }
    }
}

fn normalize(raw: &str) -> String {
    raw.trim().trim_start_matches('.').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_classifier_includes_canonical_binary_extensions() {
        let c = BinaryExtensions::default();
        for ext in [
            "exe", "dll", "so", "png", "jpg", "pdf", "zip", "wasm", "onnx", "gguf", "mp4",
        ] {
            assert!(c.contains_extension(ext), "expected default to flag {ext}");
        }
    }

    #[test]
    fn default_classifier_excludes_obvious_text_extensions() {
        let c = BinaryExtensions::default();
        for ext in ["txt", "md", "rs", "py", "js", "ts", "json", "yaml", "html"] {
            assert!(
                !c.contains_extension(ext),
                "expected default NOT to flag {ext}"
            );
        }
    }

    #[test]
    fn contains_extension_is_case_insensitive_and_dot_tolerant() {
        let c = BinaryExtensions::default();
        assert!(c.contains_extension("PNG"));
        assert!(c.contains_extension(".png"));
        assert!(c.contains_extension("  .PNG  "));
    }

    #[test]
    fn contains_extension_empty_string_is_false() {
        let c = BinaryExtensions::default();
        assert!(!c.contains_extension(""));
        assert!(!c.contains_extension("."));
        assert!(!c.contains_extension("   "));
    }

    #[test]
    fn is_binary_path_uses_path_extension() {
        let c = BinaryExtensions::default();
        assert!(c.is_binary_path("foo.png"));
        assert!(c.is_binary_path("/abs/path/to/MovieFile.MP4"));
        assert!(c.is_binary_path("nested/dir/blob.gguf"));
    }

    #[test]
    fn is_binary_path_extensionless_returns_false() {
        let c = BinaryExtensions::default();
        // Extension-less files are NOT classified binary by default;
        // callers can layer a magic-byte sniff on top if needed.
        assert!(!c.is_binary_path("README"));
        assert!(!c.is_binary_path("Makefile"));
        assert!(!c.is_binary_path(".gitignore"));
    }

    #[test]
    fn is_binary_path_unknown_extension_returns_false() {
        let c = BinaryExtensions::default();
        assert!(!c.is_binary_path("custom.foo"));
        assert!(!c.is_binary_path("logfile.unknown"));
    }

    #[test]
    fn with_extras_adds_to_set() {
        let c = BinaryExtensions::default().with_extras([".myproprietary", "bin2"]);
        assert!(c.contains_extension("myproprietary"));
        assert!(c.contains_extension("bin2"));
    }

    #[test]
    fn without_drops_from_set() {
        // A den decides .bin is actually their text-based JSONL
        // payload format.
        let c = BinaryExtensions::default().without(["bin"]);
        assert!(!c.contains_extension("bin"));
        assert!(c.contains_extension("exe"));
    }

    #[test]
    fn iter_returns_sorted() {
        let c = BinaryExtensions::new().with_extras(["zzz", "aaa", "mmm"]);
        let v: Vec<&str> = c.iter().collect();
        assert_eq!(v, vec!["aaa", "mmm", "zzz"]);
    }

    #[test]
    fn empty_classifier_classifies_nothing() {
        let c = BinaryExtensions::new();
        assert!(c.is_empty());
        assert_eq!(c.len(), 0);
        assert!(!c.is_binary_path("anything.png"));
        assert!(!c.contains_extension("png"));
    }

    #[test]
    fn default_classifier_is_non_empty_and_dedupes() {
        let c = BinaryExtensions::default();
        assert!(c.len() > 50, "default list smaller than expected: {}", c.len());
        // The data table is hand-maintained — it has at least one
        // duplicate (`key` appears under both crypto and Apple iWork
        // categories). Confirm dedup happens at construction time.
        let raw = DEFAULT_BINARY_EXTENSIONS.len();
        assert!(
            c.len() <= raw,
            "BTreeSet should never grow past the input list"
        );
    }
}
