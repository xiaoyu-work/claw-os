use super::*;
use std::fs;
use std::io::Write;

fn tmp_path(ext: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    p.push(format!("cos-system-playback-{pid}-{nanos}.{ext}"));
    p
}

fn touch(path: &Path) {
    let mut f = fs::File::create(path).expect("create tmp");
    f.write_all(&[0u8; 16]).expect("write tmp");
}

// -----------------------------------------------------------------
// PlaybackFormat::from_path
// -----------------------------------------------------------------

#[test]
fn format_from_path_recognises_known_extensions() {
    assert_eq!(
        PlaybackFormat::from_path(Path::new("foo.wav")),
        Some(PlaybackFormat::Wav)
    );
    assert_eq!(
        PlaybackFormat::from_path(Path::new("foo.WAV")),
        Some(PlaybackFormat::Wav)
    );
    assert_eq!(
        PlaybackFormat::from_path(Path::new("foo.mp3")),
        Some(PlaybackFormat::Mp3)
    );
    assert_eq!(
        PlaybackFormat::from_path(Path::new("foo.Ogg")),
        Some(PlaybackFormat::Ogg)
    );
    assert_eq!(
        PlaybackFormat::from_path(Path::new("foo.oga")),
        Some(PlaybackFormat::Ogg)
    );
    assert_eq!(
        PlaybackFormat::from_path(Path::new("foo.flac")),
        Some(PlaybackFormat::Flac)
    );
}

#[test]
fn format_from_path_rejects_unknown_extensions() {
    assert_eq!(PlaybackFormat::from_path(Path::new("foo.txt")), None);
    assert_eq!(PlaybackFormat::from_path(Path::new("foo.aac")), None);
    assert_eq!(PlaybackFormat::from_path(Path::new("foo.m4a")), None);
    assert_eq!(PlaybackFormat::from_path(Path::new("foo")), None);
}

#[test]
fn format_as_str_returns_lowercase() {
    for f in [
        PlaybackFormat::Wav,
        PlaybackFormat::Mp3,
        PlaybackFormat::Ogg,
        PlaybackFormat::Flac,
    ] {
        let s = f.as_str();
        assert_eq!(s, s.to_lowercase(), "{f:?} not lowercase");
    }
}

// -----------------------------------------------------------------
// validate_path
// -----------------------------------------------------------------

#[test]
fn play_missing_file_returns_invalid_input_path() {
    let p = tmp_path("wav");
    // ensure missing
    let _ = fs::remove_file(&p);
    let err = play_file_blocking(&p).unwrap_err();
    match err {
        SystemPlaybackError::InvalidInputPath { reason, .. } => {
            assert_eq!(reason, "file does not exist");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn play_directory_returns_invalid_input_path() {
    let dir = std::env::temp_dir();
    let err = play_file_blocking(&dir).unwrap_err();
    match err {
        SystemPlaybackError::InvalidInputPath { reason, .. } => {
            assert_eq!(reason, "path is not a regular file");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn play_unsupported_extension_rejects_before_dispatch() {
    let p = tmp_path("txt");
    touch(&p);
    let err = play_file_blocking(&p).unwrap_err();
    let _ = fs::remove_file(&p);
    match err {
        SystemPlaybackError::UnsupportedFormat { .. } => {}
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn play_no_extension_rejects_before_dispatch() {
    let mut p = std::env::temp_dir();
    let pid = std::process::id();
    p.push(format!("cos-no-ext-{pid}"));
    touch(&p);
    let err = play_file_blocking(&p).unwrap_err();
    let _ = fs::remove_file(&p);
    match err {
        SystemPlaybackError::UnsupportedFormat { .. } => {}
        other => panic!("unexpected: {other:?}"),
    }
}

// -----------------------------------------------------------------
// detect_player — validates the platform branch compiles and
// gives a sensible answer for a known-supported format.
// -----------------------------------------------------------------

#[test]
fn detect_player_compiles_for_every_format() {
    // Don't assert specific players (CI / dev box variance);
    // just confirm the call completes without panicking and
    // returns either Some/None for each format. Catches
    // `unwrap` regressions in the platform backends.
    for f in [
        PlaybackFormat::Wav,
        PlaybackFormat::Mp3,
        PlaybackFormat::Ogg,
        PlaybackFormat::Flac,
    ] {
        let _ = detect_player(f);
    }
}

#[cfg(target_os = "windows")]
#[test]
fn detect_player_windows_offers_winmm_for_wav() {
    let s = detect_player(PlaybackFormat::Wav).expect("wav backend");
    assert!(s.contains("winmm"), "got {s}");
}

#[cfg(target_os = "windows")]
#[test]
fn detect_player_windows_returns_none_for_mp3() {
    // Windows backend is WAV-only by design.
    assert_eq!(detect_player(PlaybackFormat::Mp3), None);
    assert_eq!(detect_player(PlaybackFormat::Ogg), None);
    assert_eq!(detect_player(PlaybackFormat::Flac), None);
}

#[cfg(target_os = "windows")]
#[test]
fn play_mp3_on_windows_returns_no_player_found() {
    let p = tmp_path("mp3");
    touch(&p);
    let err = play_file_blocking(&p).unwrap_err();
    let _ = fs::remove_file(&p);
    match err {
        SystemPlaybackError::NoPlayerFound { format, attempted } => {
            assert_eq!(format, PlaybackFormat::Mp3);
            assert!(!attempted.is_empty());
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[cfg(target_os = "windows")]
#[test]
fn play_truncated_wav_on_windows_returns_winmm_play_failed() {
    // `touch` writes 16 zero bytes — too short for a real WAV
    // header, so PlaySoundW will reject it. With SND_NODEFAULT
    // we get a clean failure instead of the system beep.
    let p = tmp_path("wav");
    touch(&p);
    let err = play_file_blocking(&p).unwrap_err();
    let _ = fs::remove_file(&p);
    match err {
        SystemPlaybackError::WinmmPlayFailed { .. } => {}
        other => panic!("unexpected: {other:?}"),
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn linux_explicit_override_path_is_honoured_over_auto_detect() {
    // We don't actually spawn the player — just confirm the
    // override path makes it into the diagnostic.
    std::env::set_var("COS_AUDIO_PLAYER", "/nonexistent/cos-fake-player");
    let s = detect_player(PlaybackFormat::Wav).expect("override returns Some");
    assert!(s.contains("cos-fake-player"), "got {s}");
    std::env::remove_var("COS_AUDIO_PLAYER");
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn linux_play_with_unspawnable_override_returns_configured_player_unavailable() {
    let p = tmp_path("wav");
    touch(&p);
    std::env::set_var("COS_AUDIO_PLAYER", "/nonexistent/cos-fake-player");
    let err = play_file_blocking(&p).unwrap_err();
    std::env::remove_var("COS_AUDIO_PLAYER");
    let _ = fs::remove_file(&p);
    match err {
        SystemPlaybackError::ConfiguredPlayerUnavailable { player, .. } => {
            assert!(player.to_string_lossy().contains("cos-fake-player"));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

// -----------------------------------------------------------------
// Error display
// -----------------------------------------------------------------

#[test]
fn error_display_includes_path_and_reason() {
    let e = SystemPlaybackError::InvalidInputPath {
        path: PathBuf::from("/x/y.wav"),
        reason: "file does not exist",
    };
    let s = format!("{e}");
    assert!(
        s.contains("/x/y.wav") || s.contains("\\x\\y.wav"),
        "got {s}"
    );
    assert!(s.contains("file does not exist"), "got {s}");
}

#[test]
fn error_display_unsupported_format_lists_known_extensions() {
    let e = SystemPlaybackError::UnsupportedFormat {
        path: PathBuf::from("foo.aac"),
    };
    let s = format!("{e}");
    for ext in ["wav", "mp3", "ogg", "flac"] {
        assert!(s.contains(ext), "missing {ext} in {s}");
    }
}

#[test]
fn error_display_no_player_found_lists_attempted() {
    let e = SystemPlaybackError::NoPlayerFound {
        format: PlaybackFormat::Mp3,
        attempted: vec!["foo", "bar"],
    };
    let s = format!("{e}");
    assert!(s.contains("foo"), "got {s}");
    assert!(s.contains("bar"), "got {s}");
    assert!(s.contains("mp3"), "got {s}");
}
