use std::time::Duration;

use cosmic::app::Task;
use futures::future::{AbortHandle, Abortable};

use crate::Message;
use crate::bridge::BridgeEndpoint;
use crate::fl;
use crate::recorder::{self, Recorder, RecordingMetrics};

enum VoicePhase {
    Idle,
    Recording {
        recorder: Recorder,
        generation: u64,
        metrics: RecordingMetrics,
    },
    Processing {
        generation: u64,
    },
}

pub(crate) enum VoiceTick {
    Continue,
    Stop,
    Failed(String),
}

pub(crate) struct VoiceState {
    generation: u64,
    phase: VoicePhase,
    abort: Option<AbortHandle>,
}

impl Default for VoiceState {
    fn default() -> Self {
        Self {
            generation: 0,
            phase: VoicePhase::Idle,
            abort: None,
        }
    }
}

impl VoiceState {
    pub(crate) fn is_recording(&self) -> bool {
        matches!(self.phase, VoicePhase::Recording { .. })
    }

    pub(crate) fn is_processing(&self) -> bool {
        matches!(self.phase, VoicePhase::Processing { .. })
    }

    pub(crate) fn is_active(&self) -> bool {
        !matches!(self.phase, VoicePhase::Idle)
    }

    pub(crate) fn metrics(&self) -> Option<RecordingMetrics> {
        match &self.phase {
            VoicePhase::Recording { metrics, .. } => Some(*metrics),
            VoicePhase::Idle | VoicePhase::Processing { .. } => None,
        }
    }

    pub(crate) fn start(&mut self) -> Result<(), String> {
        let recorder = Recorder::start().map_err(|error| error.to_string())?;
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let metrics = recorder.metrics();
        self.phase = VoicePhase::Recording {
            recorder,
            generation,
            metrics,
        };
        Ok(())
    }

    pub(crate) fn stop(
        &mut self,
        endpoint: Option<BridgeEndpoint>,
    ) -> Result<Task<Message>, String> {
        let phase = std::mem::replace(&mut self.phase, VoicePhase::Idle);
        let VoicePhase::Recording {
            recorder,
            generation,
            ..
        } = phase
        else {
            self.phase = phase;
            return Ok(Task::none());
        };
        let Some(endpoint) = endpoint else {
            return Err(fl!("bridge-offline"));
        };
        self.phase = VoicePhase::Processing { generation };
        let (abort_handle, abort_registration) = AbortHandle::new_pair();
        self.abort = Some(abort_handle);
        Ok(Task::perform(
            async move {
                let work = async move {
                    let wav = tokio::task::spawn_blocking(move || recorder.stop())
                        .await
                        .map_err(|error| format!("{}: {error}", fl!("recorder-task-error")))?
                        .map_err(|error| format!("{}: {error}", fl!("recording-error")))?;
                    let response = recorder::upload(endpoint, wav)
                        .await
                        .map_err(|error| format!("{}: {error}", fl!("upload-error")))?;
                    Ok((response.text, response.placeholder))
                };
                match Abortable::new(work, abort_registration).await {
                    Ok(result) => result,
                    Err(_) => Err(fl!("cancel")),
                }
            },
            move |result| cosmic::Action::App(Message::VoiceFinished { generation, result }),
        ))
    }

    pub(crate) fn cancel(&mut self) -> Task<Message> {
        self.generation = self.generation.wrapping_add(1);
        if let Some(abort) = self.abort.take() {
            abort.abort();
        }
        let phase = std::mem::replace(&mut self.phase, VoicePhase::Idle);
        match phase {
            VoicePhase::Recording { recorder, .. } => Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || recorder.cancel())
                        .await
                        .map_err(|error| error.to_string())
                        .and_then(|result| result.map_err(|error| error.to_string()))
                },
                |_| cosmic::Action::None,
            ),
            VoicePhase::Idle | VoicePhase::Processing { .. } => Task::none(),
        }
    }

    pub(crate) fn tick(&mut self) -> VoiceTick {
        let VoicePhase::Recording {
            recorder, metrics, ..
        } = &mut self.phase
        else {
            return VoiceTick::Continue;
        };
        if let Some(error) = recorder.stream_error() {
            self.generation = self.generation.wrapping_add(1);
            self.phase = VoicePhase::Idle;
            return VoiceTick::Failed(error);
        }
        *metrics = recorder.metrics();
        if metrics.elapsed >= Duration::from_secs(recorder::MAX_RECORDING_SECS) {
            VoiceTick::Stop
        } else {
            VoiceTick::Continue
        }
    }

    pub(crate) fn finish(&mut self, generation: u64) -> bool {
        let accepted = generation == self.generation
            && matches!(
                self.phase,
                VoicePhase::Processing {
                    generation: active
                } if active == generation
            );
        if accepted {
            self.abort = None;
            self.phase = VoicePhase::Idle;
        }
        accepted
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/voice.rs"));
}
