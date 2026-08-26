//! Minimal WAV (RIFF) encoder + decoder for 16-bit PCM audio.
//!
//! Dependency-free. Supports the common PCM 16-bit, mono/stereo,
//! arbitrary sample-rate case used by:
//!
//!   * Recording captures (cpal -> WAV file)
//!   * TTS provider outputs (cloud services often return MP3 or
//!     WAV; we want a stable on-disk normalisation)
//!   * STT provider inputs (most cloud APIs accept WAV PCM 16
//!     directly)
//!
//! This module deliberately doesn't try to be a full hound
//! replacement — no float samples, no extensible format chunk.
//! When richer needs arise, swap in `hound` behind a feature flag.

use super::super::MediaError;

/// Encode 16-bit signed PCM samples into a WAV (RIFF) file.
///
/// `samples` is interleaved per-frame: for stereo, `[L0, R0, L1,
/// R1, ...]`. Channel count >= 1 and sample rate > 0 must be
/// supplied by the caller.
pub fn encode_pcm16(
    samples: &[i16],
    channels: u16,
    sample_rate_hz: u32,
) -> Result<Vec<u8>, MediaError> {
    if channels == 0 {
        return Err(MediaError::InvalidRequest(
            "wav: channels must be >= 1".to_string(),
        ));
    }
    if sample_rate_hz == 0 {
        return Err(MediaError::InvalidRequest(
            "wav: sample_rate_hz must be > 0".to_string(),
        ));
    }
    if !samples.len().is_multiple_of(channels as usize) {
        return Err(MediaError::InvalidRequest(format!(
            "wav: sample count {} not divisible by channels {}",
            samples.len(),
            channels
        )));
    }

    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate_hz * channels as u32 * (bits_per_sample as u32 / 8);
    let block_align = channels * (bits_per_sample / 8);
    let data_bytes = samples.len() * 2;
    let chunk_size = 36u32
        .checked_add(data_bytes as u32)
        .ok_or_else(|| MediaError::InvalidRequest("wav: data too large".to_string()))?;

    let mut out = Vec::with_capacity(44 + data_bytes);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&chunk_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate_hz.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits_per_sample.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_bytes as u32).to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq)]
pub struct WavInfo {
    pub channels: u16,
    pub sample_rate_hz: u32,
    pub bits_per_sample: u16,
    pub samples: Vec<i16>,
}

/// Decode a WAV (RIFF, PCM 16-bit) byte buffer.
///
/// Tolerates trailing chunks (e.g. LIST/INFO) by skipping anything
/// between `fmt ` and `data`.
pub fn decode_pcm16(bytes: &[u8]) -> Result<WavInfo, MediaError> {
    if bytes.len() < 44 {
        return Err(MediaError::Parse(format!(
            "wav: too short ({} bytes)",
            bytes.len()
        )));
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(MediaError::Parse(
            "wav: missing RIFF/WAVE header".to_string(),
        ));
    }

    let mut cursor = 12usize;
    let mut fmt: Option<(u16, u32, u16)> = None;
    let mut data_start: Option<usize> = None;
    let mut data_len: Option<usize> = None;

    while cursor + 8 <= bytes.len() {
        let id = &bytes[cursor..cursor + 4];
        let chunk_size = u32::from_le_bytes(
            bytes[cursor + 4..cursor + 8]
                .try_into()
                .map_err(|_| MediaError::Parse("wav: chunk size truncated".to_string()))?,
        ) as usize;
        let body_start = cursor + 8;
        let body_end = body_start
            .checked_add(chunk_size)
            .ok_or_else(|| MediaError::Parse("wav: chunk size overflow".to_string()))?;
        if body_end > bytes.len() {
            return Err(MediaError::Parse(format!(
                "wav: chunk {:?} body exceeds buffer",
                std::str::from_utf8(id).unwrap_or("?")
            )));
        }

        if id == b"fmt " {
            if chunk_size < 16 {
                return Err(MediaError::Parse(
                    "wav: fmt chunk smaller than 16 bytes".to_string(),
                ));
            }
            let format_tag = u16::from_le_bytes([bytes[body_start], bytes[body_start + 1]]);
            if format_tag != 1 {
                return Err(MediaError::Parse(format!(
                    "wav: unsupported format tag {format_tag} (only PCM=1 supported)"
                )));
            }
            let channels = u16::from_le_bytes([bytes[body_start + 2], bytes[body_start + 3]]);
            let sample_rate = u32::from_le_bytes([
                bytes[body_start + 4],
                bytes[body_start + 5],
                bytes[body_start + 6],
                bytes[body_start + 7],
            ]);
            let bits_per_sample =
                u16::from_le_bytes([bytes[body_start + 14], bytes[body_start + 15]]);
            fmt = Some((channels, sample_rate, bits_per_sample));
        } else if id == b"data" {
            data_start = Some(body_start);
            data_len = Some(chunk_size);
            break;
        }

        // Chunks are word-aligned: pad to even length.
        let padded = body_end + (chunk_size & 1);
        cursor = padded;
    }

    let (channels, sample_rate_hz, bits_per_sample) =
        fmt.ok_or_else(|| MediaError::Parse("wav: no fmt chunk".to_string()))?;
    let data_start =
        data_start.ok_or_else(|| MediaError::Parse("wav: no data chunk".to_string()))?;
    let data_len = data_len.unwrap_or(0);

    if bits_per_sample != 16 {
        return Err(MediaError::Parse(format!(
            "wav: only 16-bit PCM supported (got {bits_per_sample})"
        )));
    }
    if channels == 0 || sample_rate_hz == 0 {
        return Err(MediaError::Parse(
            "wav: invalid channels/sample_rate".to_string(),
        ));
    }
    if !data_len.is_multiple_of(2) {
        return Err(MediaError::Parse(
            "wav: data chunk length not aligned to 16-bit samples".to_string(),
        ));
    }

    let mut samples = Vec::with_capacity(data_len / 2);
    let mut i = data_start;
    while i + 2 <= data_start + data_len {
        samples.push(i16::from_le_bytes([bytes[i], bytes[i + 1]]));
        i += 2;
    }

    Ok(WavInfo {
        channels,
        sample_rate_hz,
        bits_per_sample,
        samples,
    })
}

/// Convenience: emit a 44-byte WAV header for a zero-length stream.
/// Used by reference TTS impls that need to return a "valid empty
/// WAV" envelope.
pub fn empty_header(channels: u16, sample_rate_hz: u32) -> Vec<u8> {
    encode_pcm16(&[], channels, sample_rate_hz)
        .expect("empty_header inputs (1ch/16kHz default) are always valid")
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/media/voice/wav.rs"
    ));
}
