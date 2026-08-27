//! Bounded microphone capture and bridge voice upload.

use std::io::Cursor;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::bridge::{
    BridgeEndpoint, bridge_url, response_error, validate_response_protocol, versioned_request,
};
pub use cos_agent_protocol::VoiceResponse;

pub const TARGET_RATE: u32 = 16_000;
pub const MAX_RECORDING_SECS: u64 = 120;
const MAX_SOURCE_RATE: u32 = 192_000;
const MAX_OUTPUT_SAMPLES: usize = TARGET_RATE as usize * MAX_RECORDING_SECS as usize;

#[derive(Debug, Clone, Copy)]
pub struct RecordingMetrics {
    pub elapsed: Duration,
    pub peak: f32,
    pub samples: usize,
}

#[derive(Debug)]
struct SharedMetrics {
    started: Instant,
    peak_bits: AtomicU32,
    samples: AtomicUsize,
    stream_error: Mutex<Option<String>>,
}

enum CaptureCommand {
    Finish,
    Cancel,
}

pub struct Recorder {
    stop_tx: Option<mpsc::Sender<CaptureCommand>>,
    join: Option<JoinHandle<Result<Vec<u8>>>>,
    metrics: Arc<SharedMetrics>,
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Recorder")
            .field("metrics", &self.metrics())
            .finish_non_exhaustive()
    }
}

impl Recorder {
    pub fn start() -> Result<Self> {
        let (stop_tx, stop_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
        let metrics = Arc::new(SharedMetrics {
            started: Instant::now(),
            peak_bits: AtomicU32::new(0.0f32.to_bits()),
            samples: AtomicUsize::new(0),
            stream_error: Mutex::new(None),
        });
        let capture_metrics = Arc::clone(&metrics);
        let join = std::thread::Builder::new()
            .name("cos-agent-ui:recorder".into())
            .spawn(move || {
                let failure_tx = ready_tx.clone();
                let result = run_capture(stop_rx, ready_tx, capture_metrics);
                if let Err(error) = &result {
                    let _ = failure_tx.send(Err(anyhow!(error.to_string())));
                }
                result
            })
            .context("spawn audio capture thread")?;

        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self {
                stop_tx: Some(stop_tx),
                join: Some(join),
                metrics,
            }),
            Ok(Err(error)) => {
                let _ = join.join();
                Err(error)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => match join.join() {
                Ok(Err(error)) => Err(error),
                Ok(Ok(_)) => Err(anyhow!("audio thread exited before reporting readiness")),
                Err(_) => Err(anyhow!("audio thread panicked")),
            },
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = stop_tx.send(CaptureCommand::Cancel);
                Err(anyhow!("audio thread did not signal readiness in time"))
            }
        }
    }

    pub fn metrics(&self) -> RecordingMetrics {
        RecordingMetrics {
            elapsed: self
                .metrics
                .started
                .elapsed()
                .min(Duration::from_secs(MAX_RECORDING_SECS)),
            peak: f32::from_bits(
                self.metrics
                    .peak_bits
                    .swap(0.0f32.to_bits(), Ordering::Relaxed),
            ),
            samples: self.metrics.samples.load(Ordering::Relaxed),
        }
    }

    pub fn stream_error(&self) -> Option<String> {
        self.metrics.stream_error.lock().ok()?.clone()
    }

    pub fn stop(mut self) -> Result<Vec<u8>> {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(CaptureCommand::Finish);
        }
        let join = self
            .join
            .take()
            .ok_or_else(|| anyhow!("recorder already consumed"))?;
        join.join().map_err(|_| anyhow!("audio thread panicked"))?
    }

    pub fn cancel(mut self) -> Result<()> {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(CaptureCommand::Cancel);
        }
        let join = self
            .join
            .take()
            .ok_or_else(|| anyhow!("recorder already consumed"))?;
        join.join()
            .map_err(|_| anyhow!("audio thread panicked"))??;
        Ok(())
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(CaptureCommand::Cancel);
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn run_capture(
    stop_rx: mpsc::Receiver<CaptureCommand>,
    ready_tx: mpsc::Sender<Result<()>>,
    metrics: Arc<SharedMetrics>,
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
    let source_limit = source_rate.min(MAX_SOURCE_RATE) as usize * MAX_RECORDING_SECS as usize;
    let samples = Arc::new(Mutex::new(Vec::<f32>::with_capacity(
        (source_rate as usize * 30).min(source_limit),
    )));

    let stream = match sample_format {
        cpal::SampleFormat::F32 => {
            let samples = Arc::clone(&samples);
            let callback_metrics = Arc::clone(&metrics);
            let error_metrics = Arc::clone(&metrics);
            device.build_input_stream(
                &config,
                move |data: &[f32], _| {
                    collect_frames(
                        data,
                        channels,
                        source_limit,
                        &samples,
                        &callback_metrics,
                        |s| s,
                    )
                },
                move |error| set_stream_error(&error_metrics, error.to_string()),
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let samples = Arc::clone(&samples);
            let callback_metrics = Arc::clone(&metrics);
            let error_metrics = Arc::clone(&metrics);
            device.build_input_stream(
                &config,
                move |data: &[i16], _| {
                    collect_frames(
                        data,
                        channels,
                        source_limit,
                        &samples,
                        &callback_metrics,
                        |s| s as f32 / i16::MAX as f32,
                    )
                },
                move |error| set_stream_error(&error_metrics, error.to_string()),
                None,
            )
        }
        cpal::SampleFormat::U16 => {
            let samples = Arc::clone(&samples);
            let callback_metrics = Arc::clone(&metrics);
            let error_metrics = Arc::clone(&metrics);
            device.build_input_stream(
                &config,
                move |data: &[u16], _| {
                    collect_frames(
                        data,
                        channels,
                        source_limit,
                        &samples,
                        &callback_metrics,
                        |s| (s as f32 - 32_768.0) / 32_768.0,
                    )
                },
                move |error| set_stream_error(&error_metrics, error.to_string()),
                None,
            )
        }
        other => return Err(anyhow!("unsupported sample format: {other:?}")),
    }
    .context("build input stream")?;

    stream.play().context("start audio stream")?;
    let _ = ready_tx.send(Ok(()));
    let finish = loop {
        match stop_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(CaptureCommand::Finish) => break true,
            Ok(CaptureCommand::Cancel) | Err(mpsc::RecvTimeoutError::Disconnected) => break false,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if metrics.started.elapsed() >= Duration::from_secs(MAX_RECORDING_SECS)
            || metrics.samples.load(Ordering::Relaxed) >= source_limit
        {
            break true;
        }
        if metrics
            .stream_error
            .lock()
            .is_ok_and(|error| error.is_some())
        {
            break true;
        }
    };
    drop(stream);

    if !finish {
        return Ok(Vec::new());
    }

    if let Some(error) = metrics
        .stream_error
        .lock()
        .ok()
        .and_then(|error| error.clone())
    {
        return Err(anyhow!("audio stream error: {error}"));
    }
    let captured = std::mem::take(&mut *samples.lock().unwrap());
    let resampled = resample_to_16k(&captured, source_rate)?;
    encode_wav(&resampled)
}

fn collect_frames<T: Copy>(
    data: &[T],
    channels: usize,
    limit: usize,
    samples: &Arc<Mutex<Vec<f32>>>,
    metrics: &Arc<SharedMetrics>,
    convert: impl Fn(T) -> f32,
) {
    let mut output = samples.lock().unwrap();
    if output.len() >= limit {
        return;
    }
    let mut peak = 0.0f32;
    for frame in data.chunks(channels) {
        if output.len() >= limit || frame.len() < channels {
            break;
        }
        let mono = frame.iter().copied().map(&convert).sum::<f32>() / channels as f32;
        let mono = mono.clamp(-1.0, 1.0);
        peak = peak.max(mono.abs());
        output.push(mono);
    }
    metrics.samples.store(output.len(), Ordering::Relaxed);
    metrics.peak_bits.store(peak.to_bits(), Ordering::Relaxed);
}

fn set_stream_error(metrics: &SharedMetrics, error: String) {
    if let Ok(mut slot) = metrics.stream_error.lock() {
        *slot = Some(error);
    }
}

fn resample_to_16k(input: &[f32], source_rate: u32) -> Result<Vec<i16>> {
    if source_rate == 0 {
        return Err(anyhow!("invalid input sample rate"));
    }
    if input.is_empty() {
        return Err(anyhow!("no audio samples captured"));
    }

    let mut filtered = Vec::new();
    let source = if source_rate > TARGET_RATE {
        let window = ((source_rate as f64 / TARGET_RATE as f64).ceil() as usize).max(2);
        filtered.reserve(input.len());
        let mut sum = 0.0f64;
        for (index, sample) in input.iter().copied().enumerate() {
            sum += sample as f64;
            if index >= window {
                sum -= input[index - window] as f64;
            }
            filtered.push((sum / (index + 1).min(window) as f64) as f32);
        }
        filtered.as_slice()
    } else {
        input
    };

    let output_len = ((source.len() as u64 * TARGET_RATE as u64) / source_rate as u64)
        .max(1)
        .min(MAX_OUTPUT_SAMPLES as u64) as usize;
    let step = source_rate as f64 / TARGET_RATE as f64;
    let mut output = Vec::with_capacity(output_len);
    for index in 0..output_len {
        let position = index as f64 * step;
        let left = position.floor() as usize;
        let right = (left + 1).min(source.len() - 1);
        let fraction = (position - left as f64) as f32;
        let sample =
            source[left.min(source.len() - 1)] * (1.0 - fraction) + source[right] * fraction;
        output.push((sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
    }
    if output.is_empty() {
        return Err(anyhow!("no audio samples captured"));
    }
    Ok(output)
}

fn encode_wav(samples: &[i16]) -> Result<Vec<u8>> {
    if samples.is_empty() {
        return Err(anyhow!("no audio samples captured"));
    }
    if samples.len() > MAX_OUTPUT_SAMPLES {
        return Err(anyhow!("recording exceeds the 120 second limit"));
    }
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec).context("create WAV writer")?;
        for sample in samples {
            writer.write_sample(*sample).context("write WAV sample")?;
        }
        writer.finalize().context("finalize WAV")?;
    }
    Ok(cursor.into_inner())
}

pub async fn upload(endpoint: BridgeEndpoint, wav: Vec<u8>) -> Result<VoiceResponse> {
    let url = bridge_url(&endpoint, "/api/voice/upload");
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(135))
        .build()
        .context("build voice upload client")?;
    let (request_builder, selected) = versioned_request(client.post(&url), &endpoint)?;
    let response = request_builder
        .bearer_auth(&endpoint.token)
        .header("Content-Type", "audio/wav")
        .body(wav)
        .send()
        .await
        .context("POST /api/voice/upload")?;
    validate_response_protocol(&response, selected)?;
    if !response.status().is_success() {
        return Err(response_error(response, &url).await);
    }
    response
        .json::<VoiceResponse>()
        .await
        .context("decode voice upload response")
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/recorder.rs"
    ));
}
