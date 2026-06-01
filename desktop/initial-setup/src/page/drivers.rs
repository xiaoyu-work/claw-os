// SPDX-License-Identifier: GPL-3.0-only
//
// Initial-setup Drivers page — the final wizard step. ClawOS ships the open
// GPU stack for every card, so this page only has real work to do on a bare
// metal machine with an NVIDIA GPU: it offers to install NVIDIA's proprietary
// driver (full 3D + CUDA). On everything else (AMD/Intel, or a GPU that's
// already virtualised in WSL/containers) it reports "nothing to do" and the
// user clicks straight through.
//
// Design notes:
//
//   * All the smarts live in `/usr/lib/cos/hw/gpu-setup`, the same env-aware
//     helper the headless (WSL/Docker) first-boot service uses. The wizard is
//     a thin GUI over it: `gpu-setup detect --json` for the read-only probe,
//     and `gpu-setup install` for the action. Keeping the logic in one shell
//     helper means the GUI and the CLI can never drift.
//
//   * Detection runs in `open()` so the page is populated by the time the
//     user reaches it. It is read-only and needs no privileges.
//
//   * Installing the driver needs root (apt + DKMS). cosmic-initial-setup is
//     not guaranteed to run privileged, so we elevate through polkit:
//     `pkexec /usr/lib/cos/hw/gpu-setup install`, gated by the
//     `org.clawos.gpu-setup.install` policy shipped with the gpu-drivers
//     feature. If the wizard already happens to be root, pkexec runs the
//     command directly.
//
//   * The page is optional + skippable and always `completed()` — it must
//     never block Finish, and there is nothing to persist on Finish (the
//     install, if any, already happened when the user clicked the button).

use cosmic::iced::{Alignment, Length};
use cosmic::{Element, Task, cosmic_theme, theme, widget};

use crate::{fl, page};

/// Absolute path to the helper. Hard-coded rather than resolved via PATH so
/// the polkit action (which matches on `exec.path`) lines up exactly.
const HELPER: &str = "/usr/lib/cos/hw/gpu-setup";

/// Parsed output of `gpu-setup detect --json`. Field names mirror the JSON
/// keys the helper prints.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct Detection {
    #[serde(default)]
    pub env: String,
    #[serde(default)]
    pub gpu: String,
    #[serde(default)]
    pub gpu_name: String,
    /// One of: none | install_kmod | already | wsl_ready | wsl_missing |
    /// unsupported_arch.
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub installed: bool,
    #[serde(default)]
    pub runtime_visible: bool,
}

#[derive(Clone, Debug)]
pub enum Message {
    /// `gpu-setup detect --json` returned (or failed → None).
    Detected(Option<Detection>),
    /// User clicked the Install button.
    Install,
    /// `pkexec gpu-setup install` finished.
    Installed(Result<(), String>),
}

impl From<Message> for super::Message {
    fn from(message: Message) -> Self {
        super::Message::Drivers(message)
    }
}

impl From<Message> for crate::Message {
    fn from(message: Message) -> Self {
        crate::Message::PageMessage(message.into())
    }
}

#[derive(Default)]
pub struct Page {
    /// `None` until the detect probe returns.
    detection: Option<Detection>,
    /// A privileged install is in flight.
    installing: bool,
    /// Set once an install has completed successfully this session.
    installed_ok: bool,
    /// Last install error, surfaced inline.
    last_error: Option<String>,
}

impl Page {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, message: Message) -> Task<page::Message> {
        match message {
            Message::Detected(detection) => {
                self.detection = detection;
            }
            Message::Install => {
                if self.installing {
                    return Task::none();
                }
                self.installing = true;
                self.last_error = None;
                let fut = async move {
                    let res = tokio::task::spawn_blocking(install_blocking)
                        .await
                        .unwrap_or_else(|e| Err(format!("internal join error: {e}")));
                    page::Message::Drivers(Message::Installed(res))
                };
                return cosmic::task::future(fut);
            }
            Message::Installed(Ok(())) => {
                self.installing = false;
                self.installed_ok = true;
                if let Some(d) = self.detection.as_mut() {
                    d.action = "already".to_string();
                    d.installed = true;
                }
            }
            Message::Installed(Err(reason)) => {
                self.installing = false;
                self.last_error = Some(reason);
            }
        }
        Task::none()
    }
}

impl page::Page for Page {
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn title(&self) -> String {
        fl!("drivers-page")
    }

    fn skippable(&self) -> bool {
        true
    }

    fn optional(&self) -> bool {
        true
    }

    /// Never block Finish — the install (if any) is driven by the button,
    /// not by `apply_settings`.
    fn completed(&self) -> bool {
        true
    }

    /// Kick off the read-only detection probe when the page is shown.
    fn open(&mut self) -> Task<page::Message> {
        let fut = async move {
            let detection = tokio::task::spawn_blocking(detect_blocking)
                .await
                .unwrap_or(None);
            page::Message::Drivers(Message::Detected(detection))
        };
        cosmic::task::future(fut)
    }

    fn view(&self) -> Element<'_, page::Message> {
        let cosmic_theme::Spacing { space_s, space_m, .. } = theme::spacing();

        let description = widget::text::body(fl!("drivers-page", "description"))
            .align_x(Alignment::Center)
            .width(Length::Fill);

        let mut section = widget::settings::section();

        match &self.detection {
            None => {
                section = section.add(widget::settings::item_row(vec![
                    widget::text::body(fl!("drivers-page", "detecting")).into(),
                ]));
            }
            Some(d) => {
                // Detected-hardware line, e.g. "Detected: NVIDIA GeForce RTX 4060".
                let hw = if d.gpu == "nvidia" && !d.gpu_name.is_empty() {
                    format!("{}: {}", fl!("drivers-page", "detected"), d.gpu_name)
                } else if !d.gpu.is_empty() && d.gpu != "none" {
                    format!("{}: {}", fl!("drivers-page", "detected"), d.gpu)
                } else {
                    fl!("drivers-page", "none")
                };
                section = section.add(widget::settings::item_row(vec![
                    widget::text::body(hw).into(),
                ]));

                // Status / recommendation line keyed off the helper's action.
                let status = match d.action.as_str() {
                    "install_kmod" => Some(fl!("drivers-page", "install-available")),
                    "already" => Some(fl!("drivers-page", "already")),
                    "wsl_ready" => Some(fl!("drivers-page", "wsl-ready")),
                    "wsl_missing" => Some(fl!("drivers-page", "wsl-missing")),
                    "unsupported_arch" => Some(fl!("drivers-page", "unsupported-arch")),
                    _ => None,
                };
                if let Some(status) = status {
                    section = section.add(widget::settings::item_row(vec![
                        widget::text::body(status).into(),
                    ]));
                }

                // Install affordance — only for bare-metal NVIDIA that isn't
                // installed yet.
                if d.action == "install_kmod" && !self.installed_ok {
                    let mut btn = widget::button::standard(fl!("drivers-page", "install"));
                    if !self.installing {
                        btn = btn.on_press(page::Message::Drivers(Message::Install));
                    }
                    section = section.add(widget::settings::item_row(vec![btn.into()]));
                }

                if self.installing {
                    section = section.add(widget::settings::item_row(vec![
                        widget::text::body(fl!("drivers-page", "installing")).into(),
                    ]));
                }
                if self.installed_ok {
                    section = section.add(widget::settings::item_row(vec![
                        widget::text::body(fl!("drivers-page", "install-ok")).into(),
                    ]));
                }
                if let Some(err) = &self.last_error {
                    section = section.add(widget::settings::item_row(vec![
                        widget::text::body(format!(
                            "{}: {}",
                            fl!("drivers-page", "install-failed"),
                            err
                        ))
                        .into(),
                    ]));
                }
            }
        }

        widget::column::with_children(vec![
            description.into(),
            widget::space::vertical().height(space_s).into(),
            section.into(),
        ])
        .align_x(Alignment::Center)
        .spacing(space_m)
        .into()
    }
}

/// Read-only probe. Returns `None` if the helper is missing or its output is
/// unparseable — the view then falls back to "nothing to do".
fn detect_blocking() -> Option<Detection> {
    let out = cos_runtime::exec::run(&[HELPER, "detect", "--json"], Some(30)).ok()?;
    if out.exit_code != 0 {
        tracing::warn!(exit_code = out.exit_code, stderr = %out.stderr, "gpu-setup detect failed");
        return None;
    }
    match serde_json::from_str::<Detection>(out.stdout.trim()) {
        Ok(d) => Some(d),
        Err(why) => {
            tracing::warn!(?why, stdout = %out.stdout, "gpu-setup detect: bad JSON");
            None
        }
    }
}

/// Privileged install via polkit. A generous timeout: pulling nvidia-driver +
/// building the DKMS module against the kernel headers can take a few minutes.
fn install_blocking() -> Result<(), String> {
    match cos_runtime::exec::run(&["pkexec", HELPER, "install"], Some(1800)) {
        Ok(r) if r.exit_code == 0 => {
            tracing::info!("gpu-setup install succeeded");
            Ok(())
        }
        Ok(r) => {
            let mut summary = r.stderr.trim().to_string();
            if summary.is_empty() {
                summary = format!("exited with status {}", r.exit_code);
            }
            if summary.len() > 240 {
                summary.truncate(240);
                summary.push('…');
            }
            tracing::warn!(exit_code = r.exit_code, stderr = %r.stderr, "gpu-setup install failed");
            Err(summary)
        }
        Err(why) => {
            if why.is_denied() {
                tracing::warn!(?why, "gpu-setup install denied by claw-os-sdk");
            } else {
                tracing::error!(?why, "gpu-setup install bridge error");
            }
            Err(format!("{why}"))
        }
    }
}
