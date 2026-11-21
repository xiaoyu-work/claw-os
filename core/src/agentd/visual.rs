//! Root owns both the approval and the one-use observation in the task lease.

use crate::agent::service::{Job, Store};
use crate::agent::tools::desktop_vision::{Operation, Reply};

pub(super) struct State {
    #[cfg(target_os = "linux")]
    inner: linux::WindowSession,
}

impl State {
    pub(super) fn for_job(job: &Job, owner: u32, pid: u32) -> Option<Self> {
        #[cfg(target_os = "linux")]
        {
            if job.client.source != crate::session::SessionSource::BrokerTask
                || !job.client.local
                || job.session_id.is_none()
                || job.plan_only
                || job.delegated_from_session_id.is_some()
                || job.activity_id.is_some()
                || job.recovery_count != 0
            {
                return None;
            }
            let control = crate::display_session::registry::Registry::get()
                .and_then(|registry| registry.owner_control(owner, pid))
                .ok()?;
            Some(Self {
                inner: linux::WindowSession::new(control),
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (job, owner, pid);
            None
        }
    }

    pub(super) async fn request(
        &mut self,
        store: &Store,
        job: &Job,
        operation: Operation,
    ) -> Reply {
        #[cfg(target_os = "linux")]
        {
            return self
                .inner
                .request(store, job, operation)
                .await
                .unwrap_or_else(|message| Reply::Refused { message });
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (store, job, operation);
            Reply::Refused {
                message: "Desktop vision requires Linux".into(),
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use claw_display_control::desktop::{
        valid_observation, Command, PixelRegion, MAX_RESPONSE_BYTES, MAX_VISUAL_EDGE,
        MAX_WINDOW_EDGE, VISUAL_FRAME_TTL_MS,
    };
    use serde::Deserialize;
    use serde_json::{json, Value};
    use std::time::{Duration, Instant};

    use super::{Job, Operation, Reply, Store};
    use crate::agent::tools::desktop_vision::{Image, REQUEST_LIMIT};
    use crate::approvals::{self, RequestStatus};
    use crate::caps::{Cap, Scope, Verb};
    use crate::clawd::desktop::{parse_window_identifier, window_snapshot};
    use crate::display_session::registry::GuiControl;

    const NATIVE_TIMEOUT: Duration = Duration::from_secs(2);
    const REVIEW_TIMEOUT: Duration = Duration::from_secs(65);

    pub(super) struct WindowSession {
        display: GuiControl,
        request_count: u32,
        access: Option<Access>,
        observed: Option<Observation>,
    }

    struct Access {
        window: u64,
        generation: u32,
        expires: Instant,
    }

    struct Observation {
        id: String,
        window: u64,
        width: u32,
        height: u32,
        expires: Instant,
    }

    #[derive(Deserialize)]
    struct Frame {
        image: String,
        width: u32,
        height: u32,
        window_width: u32,
        window_height: u32,
        origin_x: u32,
        origin_y: u32,
        clickable: bool,
    }

    struct PendingReview {
        id: String,
        session: String,
        owner: u32,
        digest: String,
    }

    impl Drop for PendingReview {
        fn drop(&mut self) {
            if let Err(error) = approvals::retire_operation_decision(
                self.owner,
                &self.session,
                &self.id,
                &self.digest,
                "desktop visual review ended",
            ) {
                tracing::warn!(review_id = %self.id, %error, "could not retire desktop visual review");
            }
        }
    }

    impl WindowSession {
        pub(super) fn new(display: GuiControl) -> Self {
            Self {
                display,
                request_count: 0,
                access: None,
                observed: None,
            }
        }

        pub(super) async fn request(
            &mut self,
            store: &Store,
            job: &Job,
            operation: Operation,
        ) -> Result<Reply, String> {
            self.request_count += 1;
            live(store, job)?;
            if self.request_count > REQUEST_LIMIT || job.owner_uid != Some(self.display.owner_uid) {
                return Err("Desktop visual task budget or owner binding is unavailable".into());
            }
            let outcome = match operation {
                Operation::List => {
                    let snapshot = window_snapshot(&self.display, false).await?;
                    let windows = snapshot["windows"]
                        .as_array()
                        .ok_or("Invalid window list")?;
                    let available = windows
                        .iter()
                        .take(96)
                        .map(|window| {
                            json!({
                                "window": window["identifier"],
                                "app_id": window["app_id"].as_str().unwrap_or_default().chars()
                                    .filter(|c| !c.is_control()).take(128).collect::<String>(),
                                "active": active(window),
                                "visual_ready": has_state(window, "visual-ready"),
                            })
                        })
                        .collect::<Vec<_>>();
                    Reply::Data {
                        value: json!({"windows": available}),
                    }
                }
                Operation::Observe { window, region } => {
                    let region = region.map(|region| PixelRegion {
                        x: region.x,
                        y: region.y,
                        width: region.width,
                        height: region.height,
                    });
                    if region.is_some_and(|region| !region.valid()) {
                        return Err("Use a bounded native-pixel crop of at most 640 by 640".into());
                    }
                    self.observe(store, job, window.as_deref(), region).await?
                }
                Operation::Click {
                    observation_id,
                    x,
                    y,
                } => self.click(store, job, &observation_id, x, y).await?,
            };
            live(store, job)?;
            Ok(outcome)
        }

        async fn observe(
            &mut self,
            store: &Store,
            job: &Job,
            selected: Option<&str>,
            region: Option<PixelRegion>,
        ) -> Result<Reply, String> {
            let snapshot = window_snapshot(&self.display, false).await?;
            let target = snapshot["windows"]
                .as_array()
                .and_then(|windows| {
                    windows.iter().find(|window| {
                        active(window) && selected.is_none_or(|id| window["identifier"] == id)
                    })
                })
                .ok_or("Bring the intended window to the foreground before observing")?;
            // Older compositors must never receive an unrecognized control command.
            if !has_state(target, "visual-ready") {
                return Err("The foreground window is protected or the installed compositor does not support desktop vision".into());
            }
            let window = target["identifier"]
                .as_str()
                .ok_or("Invalid foreground window")?;
            let (epoch, native) = parse_window_identifier(window)?;
            if epoch != self.display.epoch {
                return Err("The desktop window belongs to another login".into());
            }
            let session = job
                .session_id
                .as_deref()
                .ok_or("No authenticated task session")?;
            let cap = capability(window);
            crate::workspaces::boundary::task_capability(job, &cap)?;
            let generation = approvals::generations::current(job.owner_uid, session)?;
            if !self.access.as_ref().is_some_and(|access| {
                access.window == native
                    && access.generation == generation
                    && access.expires > Instant::now()
            }) {
                self.access = None;
                self.observed = None;
                let owner = self.display.owner_uid;
                let app = display_app(target)?;
                self.access = Some(review(store, job, session, owner, native, cap, &app).await?);
            }
            self.authorized(job, native)?;
            self.observed = None;
            let observation = uuid::Uuid::new_v4().simple().to_string();
            live(store, job)?;
            let frame: Frame = serde_json::from_value(
                exchange(
                    &self.display,
                    Command::VisualCapture {
                        identifier: native,
                        observation: observation.clone(),
                        region,
                    },
                )
                .await?,
            )
            .map_err(|_| "The compositor returned an invalid window image")?;
            live(store, job)?;
            self.authorized(job, native)?;
            let (bytes, sha256) = frame.validate(region)?;
            let expires = Instant::now() + Duration::from_millis(VISUAL_FRAME_TTL_MS);
            let value = json!({
                "window": window,
                "observation_id": frame.clickable.then(|| observation.clone()),
                "width": frame.width, "height": frame.height,
                "window_width": frame.window_width, "window_height": frame.window_height,
                "origin_x": frame.origin_x, "origin_y": frame.origin_y,
                "clickable": frame.clickable,
            });
            store
                .append_stream_progress(
                    &job.id,
                    json!({
                        "kind": "desktop_visual", "action": "observe", "window": window,
                        "width": frame.width, "height": frame.height, "clickable": frame.clickable,
                        "bytes": bytes, "sha256": sha256,
                    }),
                )
                .map_err(|_| "Could not record the desktop observation")?;
            if frame.clickable {
                self.observed = Some(Observation {
                    id: observation,
                    window: native,
                    width: frame.width,
                    height: frame.height,
                    expires,
                });
            }
            Ok(Reply::Frame {
                value,
                image: Image(frame.image),
            })
        }

        async fn click(
            &mut self,
            store: &Store,
            job: &Job,
            id: &str,
            x: u32,
            y: u32,
        ) -> Result<Reply, String> {
            let observed = self.observed.take().filter(|frame| {
                valid_observation(id) && frame.id == id && frame.expires > Instant::now() && x < frame.width && y < frame.height
            }).ok_or("The observation is stale, already used, or outside its pixel bounds; observe again")?;
            self.authorized(job, observed.window)?;
            let window = format!("{}:{}", self.display.epoch.label(), observed.window);
            store.append_stream_progress(&job.id, json!({
                "kind": "desktop_visual", "action": "click_requested", "window": window, "x": x, "y": y,
            })).map_err(|_| "Could not record the desktop click request")?;
            live(store, job)?;
            let result = exchange(
                &self.display,
                Command::VisualClick {
                    identifier: observed.window,
                    observation: observed.id,
                    x,
                    y,
                },
            )
            .await?;
            if result["clicked"] != true {
                return Err(
                    "The click outcome is unconfirmed; observe again and do not replay it".into(),
                );
            }
            store.append_stream_progress(&job.id, json!({
                "kind": "desktop_visual", "action": "click_sent", "window": window, "x": x, "y": y,
            })).map_err(|_| "The click was sent, but its audit acknowledgement was lost; observe again")?;
            Ok(Reply::Data {
                value: json!({"clicked": true, "verified_effect": false, "next": "observe"}),
            })
        }

        fn authorized(&self, job: &Job, window: u64) -> Result<(), String> {
            let session = job
                .session_id
                .as_deref()
                .ok_or("No authenticated task session")?;
            let generation = approvals::generations::current(job.owner_uid, session)?;
            let identifier = format!("{}:{window}", self.display.epoch.label());
            crate::workspaces::boundary::task_capability(job, &capability(&identifier))?;
            if !self.access.as_ref().is_some_and(|access| {
                access.window == window
                    && access.expires > Instant::now()
                    && access.generation == generation
            }) {
                return Err(
                    "Desktop visual permission expired or was revoked; observe again".into(),
                );
            }
            Ok(())
        }
    }

    impl Frame {
        fn validate(&self, region: Option<PixelRegion>) -> Result<(usize, String), String> {
            if !(1..=MAX_VISUAL_EDGE).contains(&self.width)
                || !(1..=MAX_VISUAL_EDGE).contains(&self.height)
                || !(1..=MAX_WINDOW_EDGE).contains(&self.window_width)
                || !(1..=MAX_WINDOW_EDGE).contains(&self.window_height)
                || self.image.len() > MAX_RESPONSE_BYTES
                || match region {
                    Some(crop) => {
                        !self.clickable
                            || self.origin_x != crop.x
                            || self.origin_y != crop.y
                            || self.width != crop.width
                            || self.height != crop.height
                            || crop.x + crop.width > self.window_width
                            || crop.y + crop.height > self.window_height
                    }
                    None => {
                        self.origin_x != 0
                            || self.origin_y != 0
                            || self.width > self.window_width
                            || self.height > self.window_height
                            || self.clickable
                                != (self.width == self.window_width
                                    && self.height == self.window_height)
                    }
                }
            {
                return Err("The image dimensions do not match the desktop observation".into());
            }
            let png = STANDARD
                .decode(&self.image)
                .map_err(|_| "Invalid window image encoding")?;
            if !png.starts_with(b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR")
                || png.get(16..20) != Some(&self.width.to_be_bytes())
                || png.get(20..24) != Some(&self.height.to_be_bytes())
            {
                return Err("The window image does not match its PNG dimensions".into());
            }
            Ok((png.len(), crate::crypto::sha256_hex(&png)))
        }
    }

    fn active(window: &Value) -> bool {
        has_state(window, "activated")
    }

    fn has_state(window: &Value, state: &str) -> bool {
        window["states"]
            .as_array()
            .is_some_and(|states| states.iter().any(|value| value == state))
    }

    fn display_app(target: &Value) -> Result<String, String> {
        let name = target["app_id"]
            .as_str()
            .filter(|name| {
                !name.is_empty() && name.len() <= 255 && !name.chars().any(char::is_control)
            })
            .ok_or("The foreground window has no stable application identity")?;
        Ok(name.into())
    }

    fn capability(window: &str) -> Cap {
        Cap::new(
            Verb::DESKTOP_WINDOW,
            Scope::name(format!("visual:{window}")),
        )
    }

    fn live(store: &Store, job: &Job) -> Result<(), String> {
        if store
            .cancellation_requested(&job.id)
            .map_err(|_| "Could not check desktop task cancellation")?
        {
            return Err("Desktop visual task was cancelled".into());
        }
        crate::workspaces::association::check_job(job)
    }

    async fn review(
        store: &Store,
        job: &Job,
        session: &str,
        owner: u32,
        window: u64,
        cap: Cap,
        app: &str,
    ) -> Result<Access, String> {
        let digest = crate::crypto::sha256_hex(
            format!(
                "desktop-visual:{}:{}:{}",
                job.id,
                cap.scope,
                uuid::Uuid::new_v4()
            )
            .as_bytes(),
        );
        let id = approvals::submit_owned_with_context_for_operation(
            cap.verb, cap.scope.clone(), session,
            format!("Allow this task to send images of {app} to its selected model and left-click visible pixels inside that window. This does not allow full-screen capture, typing or access to other windows."),
            Some("Desktop vision".into()), Some(owner), None, Some(&digest),
        )?;
        let pending = PendingReview {
            id,
            session: session.into(),
            owner,
            digest,
        };
        let closing = Instant::now() + REVIEW_TIMEOUT;
        loop {
            live(store, job)?;
            match approvals::status_for_owner(&pending.id, Some(owner)) {
                RequestStatus::Approved => {
                    let grants = approvals::consume_grant_set_once_for_owner_operation_evidence(
                        session,
                        std::slice::from_ref(&cap),
                        Some(owner),
                        Some(&pending.digest),
                    )?
                    .ok_or("The desktop visual approval was revoked or already used")?;
                    let grant = grants
                        .first()
                        .ok_or("The desktop visual approval is unavailable")?;
                    return Ok(Access {
                        window,
                        generation: grant.generation,
                        expires: Instant::now() + grant.expires_in().min(Duration::from_secs(300)),
                    });
                }
                RequestStatus::Pending | RequestStatus::Resolving if Instant::now() < closing => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                _ => return Err("Desktop visual access was not approved in time".into()),
            }
        }
    }

    async fn exchange(display: &GuiControl, command: Command) -> Result<Value, String> {
        let display = display.clone();
        let deadline = Instant::now() + NATIVE_TIMEOUT;
        let result = tokio::task::spawn_blocking(move || display.desktop(command, deadline))
            .await
            .map_err(|_| "Native visual exchange failed; observe again before any new click")?
            .map_err(|error| {
                format!("Native visual exchange was not confirmed: {error}. Do not replay a click.")
            })?;
        if result["ok"] != true {
            return Err(result["error"]
                .as_str()
                .unwrap_or("Native visual action was refused")
                .into());
        }
        Ok(result)
    }
}
