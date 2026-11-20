use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{Error, Packet};

pub const LOGIN_SOCKET: &str = "/run/cos/display-login.sock";
pub const DISPLAY_HOST: &str = "/usr/lib/cos/bin/claw-display-host";
pub const COMPOSITOR: &str = "/usr/libexec/claw-hyprland/Hyprland";
pub const SESSION_DIRECTORY: &str = "/run/cos/display-sessions";

// Allow workload shutdown, the broker's checked GUI cleanup, and the final handshakes.
pub const BROKER_RETIRE_TIMEOUT: Duration = Duration::from_secs(5);
pub const HOST_RETIRE_TIMEOUT: Duration = Duration::from_secs(12);
pub const PAM_RETIRE_TIMEOUT: Duration = Duration::from_secs(15);
pub const MENU_LAUNCH_DIGEST: &str = "CLAW_DESKTOP_EXPECTED_DIGEST";
pub const MENU_LAUNCH_PUBLISHER: &str = "CLAW_DESKTOP_EXPECTED_PUBLISHER";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchTarget {
    pub app_id: String,
    pub digest: String,
    pub publisher: String,
}

impl LaunchTarget {
    pub fn validate(&self) -> Result<(), Error> {
        let hash = |value: &str| {
            value.strip_prefix("sha256:").is_some_and(|hex| {
                hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        };
        if self.app_id.is_empty() || self.app_id.len() > 128
            || !self.app_id.bytes().next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            || !self.app_id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || !hash(&self.digest) || !hash(&self.publisher)
        {
            return Err(Error::Protocol("invalid signed desktop launch target"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct Epoch(pub [u8; 16]);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct InstanceId(pub [u8; 16]);

impl Epoch {
    pub fn label(&self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    pub fn random() -> Result<Self, Error> {
        let mut bytes = [0; 16];
        let mut offset = 0;
        while offset < bytes.len() {
            let read = unsafe {
                libc::getrandom(bytes[offset..].as_mut_ptr().cast(), bytes.len() - offset, 0)
            };
            if read < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error.into());
            }
            if read == 0 {
                return Err(Error::Protocol("kernel randomness unavailable"));
            }
            offset += read as usize;
        }
        Ok(Self(bytes))
    }
}

impl InstanceId {
    pub fn random() -> Result<Self, Error> {
        Ok(Self(Epoch::random()?.0))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CompositorCommand {
    Initialize {
        epoch: Epoch,
        owner_uid: u32,
        audit_session: u32,
    },
    Heartbeat {
        epoch: Epoch,
        expires_monotonic_ms: u64,
    },
    Creator {
        epoch: Epoch,
        owner_uid: u32,
        instance: InstanceId,
        authority: Epoch,
        revision: u64,
        expires_monotonic_ms: u64,
        selection_read: bool,
        selection_write: bool,
        layer_shell: bool,
    },
    Refresh {
        epoch: Epoch,
        instance: InstanceId,
        authority: Epoch,
        revision: u64,
        expires_monotonic_ms: u64,
    },
    Retire {
        epoch: Epoch,
        instance: InstanceId,
    },
    SetSelection {
        epoch: Epoch,
        owner_uid: u32,
        instance: InstanceId,
        expires_monotonic_ms: u64,
    },
    Desktop {
        epoch: Epoch,
        owner_uid: u32,
        instance: InstanceId,
        expires_monotonic_ms: u64,
        command: crate::desktop::Command,
    },
    Lock {
        epoch: Epoch,
        instance: InstanceId,
        expires_monotonic_ms: u64,
    },
    Locker {
        epoch: Epoch,
        instance: InstanceId,
        expires_monotonic_ms: u64,
    },
    Shutdown {
        epoch: Epoch,
    },
}

impl Packet for CompositorCommand {
    fn descriptor_count(&self) -> usize {
        match self {
            Self::Creator { .. } => 3,
            Self::SetSelection { .. } | Self::Locker { .. } => 1,
            _ => 0,
        }
    }

    fn validate(&self) -> Result<(), Error> {
        if let Self::Desktop { command, .. } = self {
            command.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CompositorReply {
    Ready {
        epoch: Epoch,
        wayland_display: String,
    },
    CreatorReady {
        epoch: Epoch,
        instance: InstanceId,
    },
    Refreshed {
        epoch: Epoch,
        instance: InstanceId,
    },
    Retired {
        epoch: Epoch,
        instance: InstanceId,
    },
    SelectionSet {
        epoch: Epoch,
        instance: InstanceId,
    },
    DesktopResult {
        epoch: Epoch,
        instance: InstanceId,
    },
    DesktopChanged {
        epoch: Epoch,
    },
    LockerReady {
        epoch: Epoch,
        instance: InstanceId,
    },
    Locked {
        epoch: Epoch,
        instance: InstanceId,
    },
    InstanceEnded {
        epoch: Epoch,
        instance: InstanceId,
    },
    Stopped {
        epoch: Epoch,
    },
    Rejected {
        epoch: Epoch,
        instance: InstanceId,
        reason: GuiRefusal,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GuiRefusal {
    InvalidBinding,
    Unavailable,
    Expired,
}

impl Packet for CompositorReply {
    fn descriptor_count(&self) -> usize {
        usize::from(matches!(self, Self::DesktopResult { .. }))
    }

    fn validate(&self) -> Result<(), Error> {
        if let Self::Ready {
            wayland_display, ..
        } = self
        {
            if wayland_display.is_empty()
                || wayland_display.len() > 128
                || wayland_display.contains("..")
                || !wayland_display
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
            {
                return Err(Error::Protocol("invalid compositor display name"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum LoginRequest {
    Register {},
}

impl Packet for LoginRequest {
    fn descriptor_count(&self) -> usize {
        2
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum LoginReply {
    Registered { epoch: Epoch },
}

impl Packet for LoginReply {
    fn descriptor_count(&self) -> usize {
        1
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PamCommand {
    Start {
        epoch: Epoch,
        locale: crate::locale::LocaleEnvironment,
    },
    Prepared {
        epoch: Epoch,
    },
    Close {
        epoch: Epoch,
    },
    Finished {
        epoch: Epoch,
    },
}

impl Packet for PamCommand {
    fn descriptor_count(&self) -> usize {
        usize::from(matches!(self, Self::Start { .. }))
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PamReply {
    Prepared { epoch: Epoch },
    Ready { epoch: Epoch },
    Retired { epoch: Epoch },
    Failed { epoch: Epoch },
}

impl Packet for PamReply {
    fn descriptor_count(&self) -> usize {
        usize::from(matches!(self, Self::Prepared { .. }))
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum AuthorityCommand {
    Bind {
        epoch: Epoch,
        owner_uid: u32,
        audit_session: u32,
    },
    Prepared {
        epoch: Epoch,
    },
    Heartbeat {
        epoch: Epoch,
        expires_monotonic_ms: u64,
    },
    Shutdown {
        epoch: Epoch,
    },
    Retired {
        epoch: Epoch,
    },
    Gui {
        command: CompositorCommand,
    },
    Launch {
        epoch: Epoch,
        request: InstanceId,
        expires_monotonic_ms: u64,
        target: LaunchTarget,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        from_dock: bool,
    },
    ShowAgent {
        epoch: Epoch,
        request: InstanceId,
        expires_monotonic_ms: u64,
    },
}

impl Packet for AuthorityCommand {
    fn descriptor_count(&self) -> usize {
        match self {
            Self::Gui { command } => command.descriptor_count(),
            _ => 0,
        }
    }

    fn validate(&self) -> Result<(), Error> {
        if let Self::Gui { command } = self {
            if !matches!(
                command,
                CompositorCommand::Creator { .. }
                    | CompositorCommand::Refresh { .. }
                    | CompositorCommand::Retire { .. }
                    | CompositorCommand::SetSelection { .. }
                    | CompositorCommand::Desktop { .. }
                    | CompositorCommand::Lock { .. }
            ) {
                return Err(Error::Protocol("invalid GUI control command"));
            }
            command.validate()?;
        }
        if let Self::Launch { target, .. } = self {
            target.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum AuthorityReply {
    Prepared {
        epoch: Epoch,
    },
    Ready {
        epoch: Epoch,
        wayland_display: String,
    },
    Retired {
        epoch: Epoch,
    },
    Gui {
        reply: CompositorReply,
    },
    Launch {
        epoch: Epoch,
        request: InstanceId,
        submitted: bool,
    },
}

impl Packet for AuthorityReply {
    fn descriptor_count(&self) -> usize {
        match self {
            Self::Prepared { .. } => 1,
            Self::Gui { reply } => reply.descriptor_count(),
            _ => 0,
        }
    }

    fn validate(&self) -> Result<(), Error> {
        if let Self::Gui { reply } = self {
            if !matches!(
                reply,
                CompositorReply::CreatorReady { .. }
                    | CompositorReply::Refreshed { .. }
                    | CompositorReply::SelectionSet { .. }
                    | CompositorReply::DesktopResult { .. }
                    | CompositorReply::DesktopChanged { .. }
                    | CompositorReply::Locked { .. }
                    | CompositorReply::Retired { .. }
                    | CompositorReply::InstanceEnded { .. }
                    | CompositorReply::Rejected { .. }
            ) {
                return Err(Error::Protocol("invalid GUI control response"));
            }
            reply.validate()?;
        }
        if let Self::Ready {
            epoch,
            wayland_display,
        } = self
        {
            CompositorReply::Ready {
                epoch: *epoch,
                wayland_display: wayland_display.clone(),
            }
            .validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SessionRequest {
    Subscribe {},
    StartShell {},
}

impl Packet for SessionRequest {
    fn descriptor_count(&self) -> usize {
        0
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SessionEvent {
    Ready {
        epoch: Epoch,
        wayland_display: String,
    },
    Ended {
        epoch: Epoch,
    },
    ShellExited {
        epoch: Epoch,
        success: bool,
    },
}

impl Packet for SessionEvent {
    fn descriptor_count(&self) -> usize {
        0
    }

    fn validate(&self) -> Result<(), Error> {
        if let Self::Ready {
            epoch,
            wayland_display,
        } = self
        {
            CompositorReply::Ready {
                epoch: *epoch,
                wayland_display: wayland_display.clone(),
            }
            .validate()?;
        }
        Ok(())
    }
}

pub fn monotonic_ms() -> Result<u64, Error> {
    let mut time: libc::timespec = unsafe { std::mem::zeroed() };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let seconds = u64::try_from(time.tv_sec).map_err(|_| Error::Identity)?;
    seconds
        .checked_mul(1000)
        .and_then(|value| value.checked_add(time.tv_nsec as u64 / 1_000_000))
        .ok_or(Error::Identity)
}
