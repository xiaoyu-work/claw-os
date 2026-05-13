//! First-run onboarding state machine.
//!
//! Tracks which one-time setup steps the user has completed
//! across `cos agent` invocations. Library-only — the cli crate
//! drives the actual interactive prompts; this module owns the
//! persistent flag state plus the deterministic ordering of steps.
//!
//! The full step set covers: provider/model selection, credential
//! check, default skill set acceptance, MEMORY/USER seed files,
//! optional gateway opt-in. Steps marked `optional = true` count
//! as complete once skipped explicitly; the `is_complete` summary
//! ignores skipped optionals when deciding whether onboarding is
//! "done" overall.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Completed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    pub title: String,
    pub status: StepStatus,
    /// Optional steps may be skipped without blocking is_complete.
    #[serde(default)]
    pub optional: bool,
    /// Caller-supplied free-form note (e.g. selected model).
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OnboardingState {
    /// Steps in display order.
    pub steps: Vec<Step>,
}

impl OnboardingState {
    pub fn default_steps() -> Self {
        let entries = [
            ("provider", "Pick an LLM provider", false),
            ("model", "Pick a default model", false),
            ("credential", "Configure provider credentials", false),
            ("memory_seed", "Seed MEMORY.md and USER.md", true),
            ("skills_accept", "Accept default skill bundle", true),
            ("gateway", "Opt into a chat gateway (optional)", true),
        ];
        Self {
            steps: entries
                .iter()
                .map(|(id, title, opt)| Step {
                    id: (*id).to_string(),
                    title: (*title).to_string(),
                    status: StepStatus::Pending,
                    optional: *opt,
                    note: None,
                })
                .collect(),
        }
    }

    pub fn step(&self, id: &str) -> Option<&Step> {
        self.steps.iter().find(|s| s.id == id)
    }

    pub fn step_mut(&mut self, id: &str) -> Option<&mut Step> {
        self.steps.iter_mut().find(|s| s.id == id)
    }

    /// First pending step in display order, or `None` if none left.
    pub fn next_pending(&self) -> Option<&Step> {
        self.steps.iter().find(|s| s.status == StepStatus::Pending)
    }

    /// True iff every required step is Completed (Skipped is OK
    /// only when `optional == true`).
    pub fn is_complete(&self) -> bool {
        self.steps.iter().all(|s| match s.status {
            StepStatus::Completed => true,
            StepStatus::Skipped => s.optional,
            StepStatus::Pending => false,
        })
    }

    pub fn complete_step(&mut self, id: &str, note: Option<String>) -> Result<(), OnboardingError> {
        match self.step_mut(id) {
            Some(s) => {
                s.status = StepStatus::Completed;
                if note.is_some() {
                    s.note = note;
                }
                Ok(())
            }
            None => Err(OnboardingError::UnknownStep(id.to_string())),
        }
    }

    pub fn skip_step(&mut self, id: &str) -> Result<(), OnboardingError> {
        match self.step_mut(id) {
            Some(s) => {
                if !s.optional {
                    return Err(OnboardingError::CannotSkipRequired(id.to_string()));
                }
                s.status = StepStatus::Skipped;
                Ok(())
            }
            None => Err(OnboardingError::UnknownStep(id.to_string())),
        }
    }

    pub fn reset_step(&mut self, id: &str) -> Result<(), OnboardingError> {
        match self.step_mut(id) {
            Some(s) => {
                s.status = StepStatus::Pending;
                s.note = None;
                Ok(())
            }
            None => Err(OnboardingError::UnknownStep(id.to_string())),
        }
    }

    pub fn summary(&self) -> BTreeMap<String, StepStatus> {
        self.steps
            .iter()
            .map(|s| (s.id.clone(), s.status))
            .collect()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OnboardingError {
    #[error("unknown onboarding step: {0}")]
    UnknownStep(String),
    #[error("cannot skip required step: {0}")]
    CannotSkipRequired(String),
}

pub struct OnboardingStore {
    path: PathBuf,
}

impl OnboardingStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load existing state. If the file is missing or unparseable,
    /// returns the default step set so onboarding restarts cleanly.
    pub fn load(&self) -> OnboardingState {
        match fs::read_to_string(&self.path) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(state) => state,
                Err(_) => OnboardingState::default_steps(),
            },
            Err(_) => OnboardingState::default_steps(),
        }
    }

    pub fn save(&self, state: &OnboardingState) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(state)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, json)?;
        fs::rename(&tmp, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn tmp() -> PathBuf {
        std::env::temp_dir().join(format!("cos-onboarding-{}.json", Uuid::new_v4().simple()))
    }

    #[test]
    fn default_steps_have_expected_ids() {
        let s = OnboardingState::default_steps();
        let ids: Vec<&str> = s.steps.iter().map(|st| st.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "provider",
                "model",
                "credential",
                "memory_seed",
                "skills_accept",
                "gateway",
            ]
        );
    }

    #[test]
    fn next_pending_walks_in_order() {
        let mut s = OnboardingState::default_steps();
        assert_eq!(s.next_pending().unwrap().id, "provider");
        s.complete_step("provider", None).unwrap();
        assert_eq!(s.next_pending().unwrap().id, "model");
    }

    #[test]
    fn complete_step_records_note() {
        let mut s = OnboardingState::default_steps();
        s.complete_step("model", Some("gpt-5".to_string())).unwrap();
        let st = s.step("model").unwrap();
        assert_eq!(st.status, StepStatus::Completed);
        assert_eq!(st.note.as_deref(), Some("gpt-5"));
    }

    #[test]
    fn complete_unknown_returns_error() {
        let mut s = OnboardingState::default_steps();
        let err = s.complete_step("nope", None).unwrap_err();
        assert_eq!(err, OnboardingError::UnknownStep("nope".to_string()));
    }

    #[test]
    fn skip_required_step_rejected() {
        let mut s = OnboardingState::default_steps();
        let err = s.skip_step("provider").unwrap_err();
        assert!(matches!(err, OnboardingError::CannotSkipRequired(_)));
    }

    #[test]
    fn skip_optional_step_allowed() {
        let mut s = OnboardingState::default_steps();
        s.skip_step("gateway").unwrap();
        assert_eq!(s.step("gateway").unwrap().status, StepStatus::Skipped);
    }

    #[test]
    fn is_complete_requires_all_required_done() {
        let mut s = OnboardingState::default_steps();
        assert!(!s.is_complete());
        for id in ["provider", "model", "credential"] {
            s.complete_step(id, None).unwrap();
        }
        // Optional ones still pending — not complete.
        assert!(!s.is_complete());
        // Skip optionals.
        s.skip_step("memory_seed").unwrap();
        s.skip_step("skills_accept").unwrap();
        s.skip_step("gateway").unwrap();
        assert!(s.is_complete());
    }

    #[test]
    fn is_complete_accepts_completed_optionals() {
        let mut s = OnboardingState::default_steps();
        for id in [
            "provider",
            "model",
            "credential",
            "memory_seed",
            "skills_accept",
            "gateway",
        ] {
            s.complete_step(id, None).unwrap();
        }
        assert!(s.is_complete());
    }

    #[test]
    fn reset_step_clears_status_and_note() {
        let mut s = OnboardingState::default_steps();
        s.complete_step("model", Some("gpt-5".to_string())).unwrap();
        s.reset_step("model").unwrap();
        let st = s.step("model").unwrap();
        assert_eq!(st.status, StepStatus::Pending);
        assert!(st.note.is_none());
    }

    #[test]
    fn store_round_trip() {
        let p = tmp();
        let store = OnboardingStore::new(&p);
        let mut s = store.load();
        s.complete_step("provider", Some("openai".to_string()))
            .unwrap();
        store.save(&s).unwrap();
        let reloaded = store.load();
        assert_eq!(
            reloaded.step("provider").unwrap().status,
            StepStatus::Completed
        );
        fs::remove_file(&p).ok();
    }

    #[test]
    fn store_load_missing_returns_default_steps() {
        let p = tmp();
        let store = OnboardingStore::new(&p);
        let s = store.load();
        assert_eq!(s.steps.len(), 6);
    }

    #[test]
    fn store_load_garbage_falls_back_to_default_steps() {
        let p = tmp();
        fs::create_dir_all(p.parent().unwrap()).ok();
        fs::write(&p, "{not json").unwrap();
        let store = OnboardingStore::new(&p);
        let s = store.load();
        assert_eq!(s.steps.len(), 6);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn summary_returns_id_to_status_map() {
        let mut s = OnboardingState::default_steps();
        s.complete_step("provider", None).unwrap();
        let m = s.summary();
        assert_eq!(m["provider"], StepStatus::Completed);
        assert_eq!(m["model"], StepStatus::Pending);
    }
}
