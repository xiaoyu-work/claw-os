//! `system_playback` — short-term, OS-level audio file playback.
//!
//! This is a **stopgap** module, deliberately not named
//! `voice/playback.rs`. The eventual canonical playback surface will
//! be a cpal-backed pipeline that supports streaming, stop/pause
//! handles, and raw-PCM ingestion. This module ships the narrowest
//! possible thing that lets `cos` *play a file from disk and block
//! until done*, using whatever audio facility the host OS exposes
//! natively.
//!
//! ## Backend matrix
//!
//!   * **Windows** — `PlaySoundW` from `winmm.dll` (statically linked
//!     by the OS loader, ships with every Windows install). WAV only.
//!   * **macOS**   — `afplay` (built into every macOS since 10.5).
//!     Handles WAV / MP3 / AIFF / M4A / CAF natively.
//!   * **Linux**   — explicit `COS_AUDIO_PLAYER` env-var override
//!     (one binary path) takes precedence; otherwise format-aware
//!     fallback through a small list of standard CLI players.
//!     `aplay` is intentionally never picked for non-WAV.
//!
//! ## Non-goals
//!
//!   * Playing raw PCM frames. Use cpal-backed playback once landed.
//!   * Stop / pause handles. `play_file_blocking` is **blocking** —
//!     wrap it in `tokio::task::spawn_blocking` if the caller needs
//!     concurrency.
//!   * Format conversion. Caller hands us a file in one of the four
//!     extensions we recognise (`wav`, `mp3`, `ogg`, `flac`); we
//!     reject everything else up-front rather than trying to be
//!     clever via file association.
//!
//! ## Why we don't shell out to `cmd /c start` on Windows
//!
//! `start` opens the file via the registered association — which can
//! be any user-installed app, GUI editor, browser, etc. It returns
//! before audio actually finishes, and `start /wait` only waits for
//! the launched process, not for playback. That makes our
//! `play_file_blocking()` contract impossible to honour. `winmm`'s
//! `PlaySoundW` is the canonical Win32 API, ships in every Windows
//! since 95, blocks until completion, and supports nothing but WAV
//! — which is exactly what we want for a stopgap.
//!
//! ## Subprocess timeouts and cancellation
//!
//! All CLI-backed playback paths (afplay / paplay / ffplay / mpg123 /
//! aplay / `COS_AUDIO_PLAYER`) hand the file to a child process via
//! `Command::spawn`, then poll with a timeout. If the timeout fires
//! the child is killed and `PlayerTimedOut` is returned; if the
//! caller drops the future / panics during a blocking wait, a
//! `ChildGuard` ensures the child is also killed so we don't leak
//! orphan audio processes that keep speaking after the agent has
//! moved on.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Hard upper bound on a single playback subprocess. Real-world
/// utterances are < 60 s; the agent runs many short cycles, so
/// capping the worst case at 5 minutes prevents a misconfigured
/// player from blocking the whole agent loop on a 10-hour file.
pub const PLAYER_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Polling interval for the child-wait loop. 100 ms keeps CPU near
/// zero while still letting us cancel within a perceptual blink.
const PLAYER_POLL: Duration = Duration::from_millis(100);

/// Drop guard around a spawned `Child`. Ensures any pending child is
/// killed and reaped if the calling function panics or exits via
/// `?` before we call `into_inner` to disarm the guard.
struct ChildGuard {
    inner: Option<std::process::Child>,
}

impl ChildGuard {
    fn new(child: std::process::Child) -> Self {
        Self { inner: Some(child) }
    }
    fn child_mut(&mut self) -> &mut std::process::Child {
        self.inner.as_mut().expect("child still present")
    }
    fn disarm(mut self) -> std::process::Child {
        self.inner.take().expect("child still present")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut c) = self.inner.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Spawn `cmd`, wait up to [`PLAYER_TIMEOUT`] for it to exit, and
/// return its captured stdout/stderr. Kills the child on timeout
/// or on early return through the [`ChildGuard`].
#[cfg(any(target_os = "macos", all(unix, not(target_os = "macos"))))]
fn run_player_command(
    player_display: String,
    mut cmd: std::process::Command,
) -> Result<(), SystemPlaybackError> {
    use std::process::Stdio;
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = cmd.spawn().map_err(|e| {
        SystemPlaybackError::ConfiguredPlayerUnavailable {
            player: PathBuf::from(&player_display),
            source: e,
        }
    })?;
    let mut guard = ChildGuard::new(child);
    let started = Instant::now();
    let status = loop {
        match guard.child_mut().try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                if started.elapsed() >= PLAYER_TIMEOUT {
                    // ChildGuard's Drop will kill+reap as we
                    // return — that's exactly the semantics we want.
                    return Err(SystemPlaybackError::PlayerTimedOut {
                        player: player_display.clone(),
                        after: PLAYER_TIMEOUT,
                    });
                }
                std::thread::sleep(PLAYER_POLL);
            }
            Err(e) => return Err(SystemPlaybackError::Io(e)),
        }
    };
    // Disarm so the kill doesn't fire on already-exited child.
    let child = guard.disarm();
    let out = child.wait_with_output().map_err(SystemPlaybackError::Io)?;
    if !status.success() {
        return Err(SystemPlaybackError::PlayerFailed {
            player: player_display,
            code: status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(())
}

// =====================================================================
// Public types
// =====================================================================

/// File extensions this stopgap recognises. Anything else gets
/// rejected with `Unsupported`. We deliberately do NOT sniff
/// magic bytes — extension is a tighter contract for a stopgap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackFormat {
    Wav,
    Mp3,
    Ogg,
    Flac,
}

impl PlaybackFormat {
    /// Map a path to its `PlaybackFormat`. Returns `None` if the
    /// extension is missing, unrecognised, or non-ASCII.
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "wav" => Some(Self::Wav),
            "mp3" => Some(Self::Mp3),
            "ogg" | "oga" => Some(Self::Ogg),
            "flac" => Some(Self::Flac),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Mp3 => "mp3",
            Self::Ogg => "ogg",
            Self::Flac => "flac",
        }
    }
}

#[derive(Debug)]
pub enum SystemPlaybackError {
    /// Path doesn't exist, or doesn't point to a regular file.
    InvalidInputPath { path: PathBuf, reason: &'static str },

    /// Extension not in our allowlist (or missing).
    UnsupportedFormat { path: PathBuf },

    /// Format would work, but no usable player was found on PATH.
    /// `attempted` records the binaries we tried, for diagnostic
    /// messages like "install one of: paplay, ffplay, mpg123".
    NoPlayerFound {
        format: PlaybackFormat,
        attempted: Vec<&'static str>,
    },

    /// The configured `COS_AUDIO_PLAYER` binary couldn't be spawned
    /// (missing executable / permission denied / etc).
    ConfiguredPlayerUnavailable { player: PathBuf, source: io::Error },

    /// A spawned player exited non-zero.
    PlayerFailed {
        player: String,
        code: Option<i32>,
        stderr: String,
    },

    /// Win32 `PlaySoundW` returned 0 (failure). Windows-only path;
    /// the OS doesn't surface a richer code through this API.
    WinmmPlayFailed { path: PathBuf },

    /// The spawned player did not exit within the configured timeout
    /// and was killed. Indicates a hung player or an audio file far
    /// larger than expected.
    PlayerTimedOut { player: String, after: Duration },

    /// Anything else that happens during dispatch.
    Io(io::Error),
}

impl fmt::Display for SystemPlaybackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInputPath { path, reason } => {
                write!(f, "invalid input path '{}': {}", path.display(), reason)
            }
            Self::UnsupportedFormat { path } => write!(
                f,
                "unsupported audio format for '{}'; expected one of wav, mp3, ogg, flac",
                path.display()
            ),
            Self::NoPlayerFound { format, attempted } => write!(
                f,
                "no audio player found for {} (tried: {})",
                format.as_str(),
                attempted.join(", ")
            ),
            Self::ConfiguredPlayerUnavailable { player, source } => write!(
                f,
                "COS_AUDIO_PLAYER '{}' could not be spawned: {source}",
                player.display()
            ),
            Self::PlayerFailed {
                player,
                code,
                stderr,
            } => {
                let code_s = code.map(|c| c.to_string()).unwrap_or_else(|| "?".into());
                write!(
                    f,
                    "audio player '{player}' exited with status {code_s}: {}",
                    stderr.trim()
                )
            }
            Self::WinmmPlayFailed { path } => write!(
                f,
                "winmm PlaySoundW failed for '{}' (path may be too long, file invalid, or device unavailable)",
                path.display()
            ),
            Self::PlayerTimedOut { player, after } => write!(
                f,
                "audio player '{player}' did not exit within {} s and was killed",
                after.as_secs()
            ),
            Self::Io(e) => write!(f, "io error during playback: {e}"),
        }
    }
}

impl std::error::Error for SystemPlaybackError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ConfiguredPlayerUnavailable { source, .. } => Some(source),
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for SystemPlaybackError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

// =====================================================================
// Public entrypoint
// =====================================================================

/// Play `path` and block the calling thread until playback finishes.
///
/// `path` must be a regular file with one of these extensions:
/// `wav`, `mp3`, `ogg`, `oga`, `flac`. Anything else returns
/// `UnsupportedFormat` without spawning anything.
///
/// On Windows, only WAV is supported — MP3/OGG/FLAC will fall back
/// through the platform error path because `PlaySoundW` rejects
/// them. We could add more formats later via Media Foundation, but
/// that's deliberately out of scope for the stopgap.
///
/// Wrap in `tokio::task::spawn_blocking` if you need to await this
/// from an async context.
pub fn play_file_blocking(path: &Path) -> Result<(), SystemPlaybackError> {
    validate_path(path)?;
    let format =
        PlaybackFormat::from_path(path).ok_or_else(|| SystemPlaybackError::UnsupportedFormat {
            path: path.to_path_buf(),
        })?;
    backend::play(path, format)
}

fn validate_path(path: &Path) -> Result<(), SystemPlaybackError> {
    let meta = match path.metadata() {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(SystemPlaybackError::InvalidInputPath {
                path: path.to_path_buf(),
                reason: "file does not exist",
            });
        }
        Err(e) => return Err(SystemPlaybackError::Io(e)),
    };
    if !meta.is_file() {
        return Err(SystemPlaybackError::InvalidInputPath {
            path: path.to_path_buf(),
            reason: "path is not a regular file",
        });
    }
    Ok(())
}

// =====================================================================
// Diagnostics — surfaced by `cos agent media playback-status`
// =====================================================================

/// Human-readable summary of which backend / player would be used
/// for a given format on this host. Returns `None` if no usable
/// backend is available.
pub fn detect_player(format: PlaybackFormat) -> Option<String> {
    backend::detect(format)
}

// =====================================================================
// Platform backends
// =====================================================================

#[cfg(target_os = "windows")]
mod backend {
    use super::*;

    pub fn play(path: &Path, format: PlaybackFormat) -> Result<(), SystemPlaybackError> {
        if format != PlaybackFormat::Wav {
            return Err(SystemPlaybackError::NoPlayerFound {
                format,
                attempted: vec!["winmm:PlaySoundW (WAV-only)"],
            });
        }
        winmm_play(path)
    }

    pub fn detect(format: PlaybackFormat) -> Option<String> {
        if format == PlaybackFormat::Wav {
            Some("winmm:PlaySoundW".to_string())
        } else {
            None
        }
    }

    // -----------------------------------------------------------------
    // PlaySoundW thin FFI
    // -----------------------------------------------------------------
    //
    // PlaySoundW(LPCWSTR pszSound, HMODULE hmod, DWORD fdwSound) -> BOOL
    //
    // We use SND_FILENAME | SND_SYNC | SND_NODEFAULT.
    //
    //   * SND_FILENAME (0x00020000): pszSound is a filename, not a
    //     resource id or alias.
    //   * SND_SYNC (0x00000000): block until playback completes.
    //   * SND_NODEFAULT (0x00000002): if the file is unplayable,
    //     return failure instead of playing the default beep —
    //     a notorious silent-failure footgun otherwise.
    //
    // Linker: we add `winmm` to the build via the `link` attribute
    // below; it's present on every Windows install since 95 and
    // ships with the SDK / MinGW out of the box.

    use std::ffi::c_void;

    const SND_SYNC: u32 = 0x0000_0000;
    const SND_NODEFAULT: u32 = 0x0000_0002;
    const SND_FILENAME: u32 = 0x0002_0000;

    #[link(name = "winmm")]
    extern "system" {
        fn PlaySoundW(pszSound: *const u16, hmod: *const c_void, fdwSound: u32) -> i32;
    }

    fn winmm_play(path: &Path) -> Result<(), SystemPlaybackError> {
        // Canonicalise then convert to UTF-16 NUL-terminated. Avoid
        // panicking on invalid UTF-16 — `OsStrExt::encode_wide` is
        // lossless on Windows so this is just a vector build.
        use std::os::windows::ffi::OsStrExt;

        let canon = std::fs::canonicalize(path).map_err(SystemPlaybackError::Io)?;
        let mut wide: Vec<u16> = canon.as_os_str().encode_wide().collect();
        wide.push(0);

        let ok = unsafe {
            PlaySoundW(
                wide.as_ptr(),
                std::ptr::null(),
                SND_SYNC | SND_NODEFAULT | SND_FILENAME,
            )
        };
        if ok == 0 {
            return Err(SystemPlaybackError::WinmmPlayFailed { path: canon });
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod backend {
    use super::*;
    use std::process::Command;

    const PLAYER: &str = "afplay";

    pub fn play(path: &Path, format: PlaybackFormat) -> Result<(), SystemPlaybackError> {
        // afplay handles wav / mp3 / aiff / m4a / caf / aac. flac and
        // ogg are not in the supported set on stock macOS — let
        // callers know up-front rather than wait for a non-zero exit.
        if matches!(format, PlaybackFormat::Flac | PlaybackFormat::Ogg) {
            return Err(SystemPlaybackError::NoPlayerFound {
                format,
                attempted: vec![PLAYER],
            });
        }
        run_simple(PLAYER, &[path.as_os_str()])
    }

    pub fn detect(format: PlaybackFormat) -> Option<String> {
        match format {
            PlaybackFormat::Wav | PlaybackFormat::Mp3 => Some(PLAYER.to_string()),
            _ => None,
        }
    }

    fn run_simple(player: &str, args: &[&std::ffi::OsStr]) -> Result<(), SystemPlaybackError> {
        let mut cmd = Command::new(player);
        cmd.args(args);
        super::run_player_command(player.to_string(), cmd)
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod backend {
    use super::*;
    use std::ffi::OsStr;
    use std::path::PathBuf;
    use std::process::Command;

    /// `(player, supports_format)` — order matters; first match wins.
    /// `aplay` is WAV-only by design so we never offer it for
    /// compressed formats. `paplay` (PulseAudio) handles WAV and
    /// natively-supported FLAC / OGG. `mpg123` is the canonical MP3
    /// CLI. `ffplay` is the universal fallback when ffmpeg's around.
    const CHAIN: &[(&str, fn(PlaybackFormat) -> bool)] = &[
        ("paplay", supports_paplay),
        ("ffplay", |_| true),
        ("mpg123", |f| f == PlaybackFormat::Mp3),
        ("aplay", |f| f == PlaybackFormat::Wav),
    ];

    fn supports_paplay(f: PlaybackFormat) -> bool {
        matches!(
            f,
            PlaybackFormat::Wav | PlaybackFormat::Ogg | PlaybackFormat::Flac
        )
    }

    pub fn play(path: &Path, format: PlaybackFormat) -> Result<(), SystemPlaybackError> {
        // 1. Honour an explicit override first. We don't try to
        //    second-guess what the user configured — if it fails to
        //    spawn, we surface that directly.
        if let Some(player) = std::env::var_os("COS_AUDIO_PLAYER") {
            let player_path = PathBuf::from(&player);
            return run(player_path, path);
        }

        // 2. Auto-select a format-capable player from PATH.
        let mut attempted: Vec<&'static str> = Vec::new();
        for (bin, supports) in CHAIN {
            if !supports(format) {
                continue;
            }
            attempted.push(*bin);
            if which(bin) {
                return run_chain_player(bin, path, format);
            }
        }
        Err(SystemPlaybackError::NoPlayerFound { format, attempted })
    }

    pub fn detect(format: PlaybackFormat) -> Option<String> {
        if let Some(p) = std::env::var_os("COS_AUDIO_PLAYER") {
            return Some(format!("COS_AUDIO_PLAYER={}", PathBuf::from(p).display()));
        }
        for (bin, supports) in CHAIN {
            if supports(format) && which(bin) {
                return Some(bin.to_string());
            }
        }
        None
    }

    fn run(player_path: PathBuf, target: &Path) -> Result<(), SystemPlaybackError> {
        let bin_name = player_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        let args: &[&OsStr] = &[target.as_os_str()];
        let mut cmd = Command::new(&player_path);
        cmd.args(args);
        super::run_player_command(bin_name, cmd)
    }

    fn run_chain_player(
        bin: &str,
        target: &Path,
        _format: PlaybackFormat,
    ) -> Result<(), SystemPlaybackError> {
        // `ffplay` is GUI by default — force its headless flags.
        let args_owned: Vec<&OsStr> = if bin == "ffplay" {
            vec![
                OsStr::new("-nodisp"),
                OsStr::new("-autoexit"),
                OsStr::new("-loglevel"),
                OsStr::new("error"),
                target.as_os_str(),
            ]
        } else {
            vec![target.as_os_str()]
        };
        let mut cmd = Command::new(bin);
        cmd.args(&args_owned);
        super::run_player_command(bin.to_string(), cmd)
    }

    fn which(bin: &str) -> bool {
        let path = match std::env::var_os("PATH") {
            Some(p) => p,
            None => return false,
        };
        for dir in std::env::split_paths(&path) {
            if dir.join(bin).is_file() {
                return true;
            }
        }
        false
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/media/voice/system_playback.rs"
    ));
}
