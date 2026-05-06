//! Voice subsystem: recording, playback, WAV codec.
//!
//! This commit lands the dependency-free WAV encoder/decoder
//! helpers used by both record and playback paths. cpal-driven
//! recording and playback land in follow-up commits behind a
//! cargo feature once the audio backend dependency is added.
//!
//! `system_playback` is a stopgap blocking playback surface that
//! uses the OS's built-in audio facilities (Win32 `PlaySoundW` on
//! Windows, `afplay` on macOS, format-aware CLI players on Linux).
//! It does not require cpal and is exposed so callers can play
//! WAVs / TTS output today, ahead of the cpal-backed real
//! pipeline.

pub mod system_playback;
pub mod wav;
