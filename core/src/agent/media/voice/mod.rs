//! Voice subsystem: recording, playback, WAV codec.
//!
//! This commit lands the dependency-free WAV encoder/decoder
//! helpers used by both record and playback paths. cpal-driven
//! recording and playback land in follow-up commits behind a
//! cargo feature once the audio backend dependency is added.

pub mod wav;
