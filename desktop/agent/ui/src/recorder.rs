//! Microphone capture for voice input.
//!
//! Replaces the React `useAudioRecording` hook (which used the
//! browser's MediaRecorder + getUserMedia). We use `cpal` for
//! cross-platform capture and `hound` to encode the raw samples into
//! a 16 kHz mono PCM WAV in memory, then upload them to the bridge's
//! `/api/voice/upload` endpoint with `Content-Type: audio/wav`.
//!
//! Design notes:
//!
//! * `cpal::Stream` is `!Send`. We park it on a dedicated std::thread
//!   that owns the stream and an accumulating sample buffer. A oneshot
//!   `std::sync::mpsc::channel` is the stop signal — when the main
//!   thread drops the recorder, the audio thread tears down cleanly.
//!
//! * 16 kHz mono i16 is what most STT models want (Whisper, Vosk,
//!   etc.) and matches the bridge's documented contract. We resample
//!   in software by choosing the cheapest input format the device
//!   offers (typically f32, sometimes i16) and converting frame-by-
//!   frame; if the device's native rate isn't 16 kHz we downsample
//!   with a simple decimating filter — good enough for speech.

use std::io::Cursor;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::bridge::BridgeEndpoint;

/// Target sample rate for the WAV upload. Most STT backends expect
/// 16 kHz; uploading higher only wastes bandwidth.
const TARGET_RATE: u32 = 16_000;

/// In-progress recording handle. Drop or call [`Recorder::stop`] to
/// finalize.
pub struct Recorder {
    stop_tx: Option<mpsc::Sender<()>>,
    join: Option<JoinHandle<Result<Vec<u8>>>>,
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder").finish_non_exhaustive()
    }
}

impl Recorder {
    /// Start capturing from the default input device. Returns
    /// immediately; samples accumulate on a background thread until
    /// [`Self::stop`] is called.
    pub fn start() -> Result<Self> {
        let (stop_tx, stop_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();

        let join = std::thread::Builder::new()
            .name("cos-agent-ui:recorder".into())
            .spawn(move || run_capture(stop_rx, ready_tx))
            .context("spawn audio capture thread")?;

        // Block briefly to surface device-open errors synchronously
        // (e.g. "no input device" or "permission denied"). 5s upper
        // bound is generous — cpal's device open is typically <50ms.
        match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Recorder {
                stop_tx: Some(stop_tx),
                join: Some(join),
            }),
            Ok(Err(e)) => {
                // The thread will exit on its own; just wait briefly.
                let _ = join.join();
                Err(e)
            }
            Err(_) => Err(anyhow!("audio thread did not signal readiness in time")),
        }
    }

    /// Stop recording and return the encoded WAV bytes.
    ///
    /// Blocks until the audio thread has finished encoding — should
    /// be called from `tokio::task::spawn_blocking` (or a background
    /// thread) rather than directly from the UI event loop.
    pub fn stop(mut self) -> Result<Vec<u8>> {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        let join = self
            .join
            .take()
            .ok_or_else(|| anyhow!("recorder already consumed"))?;
        join.join().map_err(|_| anyhow!("audio thread panicked"))?
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn run_capture(
    stop_rx: mpsc::Receiver<()>,
    ready_tx: mpsc::Sender<Result<()>>,
) -> Result<Vec<u8>> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("no default input device"))?;

    let supported = device
        .default_input_config()
        .context("query default input config")?;
    let sample_format = supported.sample_format();
    let source_rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;
    let config: cpal::StreamConfig = supported.into();

    let samples: Arc<Mutex<Vec<i16>>> = Arc::new(Mutex::new(Vec::with_capacity(
        TARGET_RATE as usize * 30,
    )));
    let samples_for_cb = Arc::clone(&samples);

    // The downsampler is a coarse decimator. For source_rate/TARGET_RATE
    // ratios that aren't whole numbers we accumulate a fractional
    // counter and emit a sample whenever it crosses 1.0. This is
    // sufficient for speech-band signals; STT models do their own
    // proper resampling internally if they care.
    let ratio = source_rate as f64 / TARGET_RATE as f64;
    let down_counter = Arc::new(Mutex::new(0.0f64));
    let down_counter_cb = Arc::clone(&down_counter);
    let err_fn = |e| tracing::warn!("audio stream error: {e}");

    let stream = match sample_format {
        cpal::SampleFormat::F32 => device
            .build_input_stream(
                &config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let mut buf = samples_for_cb.lock().unwrap();
                    let mut counter = down_counter_cb.lock().unwrap();
                    for frame in data.chunks(channels) {
                        // Mix down to mono by averaging channels.
                        let sum: f32 = frame.iter().copied().sum();
                        let mono = sum / channels as f32;
                        *counter += 1.0;
                        if *counter >= ratio {
                            *counter -= ratio;
                            let clipped = mono.clamp(-1.0, 1.0);
                            buf.push((clipped * i16::MAX as f32) as i16);
                        }
                    }
                },
                err_fn,
                None,
            )
            .context("build f32 input stream")?,
        cpal::SampleFormat::I16 => device
            .build_input_stream(
                &config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    let mut buf = samples_for_cb.lock().unwrap();
                    let mut counter = down_counter_cb.lock().unwrap();
                    for frame in data.chunks(channels) {
                        let sum: i32 = frame.iter().map(|&s| s as i32).sum();
                        let mono = (sum / channels as i32) as i16;
                        *counter += 1.0;
                        if *counter >= ratio {
                            *counter -= ratio;
                            buf.push(mono);
                        }
                    }
                },
                err_fn,
                None,
            )
            .context("build i16 input stream")?,
        cpal::SampleFormat::U16 => device
            .build_input_stream(
                &config,
                move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    let mut buf = samples_for_cb.lock().unwrap();
                    let mut counter = down_counter_cb.lock().unwrap();
                    for frame in data.chunks(channels) {
                        let sum: i32 = frame.iter().map(|&s| s as i32 - 32_768).sum();
                        let mono = (sum / channels as i32) as i16;
                        *counter += 1.0;
                        if *counter >= ratio {
                            *counter -= ratio;
                            buf.push(mono);
                        }
                    }
                },
                err_fn,
                None,
            )
            .context("build u16 input stream")?,
        other => return Err(anyhow!("unsupported sample format: {other:?}")),
    };

    stream.play().context("start audio stream")?;
    let _ = ready_tx.send(Ok(()));

    // Park until the controller drops or signals stop.
    let _ = stop_rx.recv();
    drop(stream);

    let captured = std::mem::take(&mut *samples.lock().unwrap());
    encode_wav(&captured)
}

fn encode_wav(samples: &[i16]) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::<u8>::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec)
            .context("create WAV writer")?;
        for &s in samples {
            writer.write_sample(s).context("write WAV sample")?;
        }
        writer.finalize().context("finalize WAV")?;
    }
    Ok(cursor.into_inner())
}

/// Response shape from `POST /api/voice/upload` (matches
/// `bridge/src/routes/voice.rs::VoiceResponse`).
#[derive(Debug, serde::Deserialize)]
pub struct VoiceResponse {
    pub text: String,
    #[serde(default)]
    pub placeholder: bool,
    #[serde(default)]
    pub error: Option<String>,
}

/// POST a recorded WAV blob to the bridge and decode the JSON
/// response. The bridge returns a placeholder transcript when no
/// STT backend is wired — we surface that to the user as a tip
/// rather than dropping it silently into the input.
pub async fn upload(endpoint: BridgeEndpoint, wav: Vec<u8>) -> Result<VoiceResponse> {
    let url = format!(
        "http://127.0.0.1:{}/api/voice/upload",
        endpoint.port
    );
    let resp = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&endpoint.token)
        .header("Content-Type", "audio/wav")
        .body(wav)
        .send()
        .await
        .context("POST /api/voice/upload")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("voice upload failed ({status}): {body}"));
    }
    let json: VoiceResponse = resp
        .json()
        .await
        .context("decode voice upload response")?;
    Ok(json)
}
