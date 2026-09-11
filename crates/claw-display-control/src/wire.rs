use serde::{Deserialize, Serialize};

use crate::{Error, Packet};

pub const LOGIN_SOCKET: &str = "/run/cos/display-login.sock";
pub const DISPLAY_HOST: &str = "/usr/lib/cos/bin/claw-display-host";
pub const COMPOSITOR: &str = "/usr/bin/cosmic-comp";
pub const SESSION_DIRECTORY: &str = "/run/cos/display-sessions";

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
    Shutdown {
        epoch: Epoch,
    },
}

impl Packet for CompositorCommand {
    fn descriptor_count(&self) -> usize {
        if matches!(self, Self::Creator { .. }) {
            3
        } else {
            0
        }
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
        0
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
            ) {
                return Err(Error::Protocol("invalid GUI control command"));
            }
            command.validate()?;
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
}

impl Packet for AuthorityReply {
    fn descriptor_count(&self) -> usize {
        usize::from(matches!(self, Self::Prepared { .. }))
    }

    fn validate(&self) -> Result<(), Error> {
        if let Self::Gui { reply } = self {
            if !matches!(
                reply,
                CompositorReply::CreatorReady { .. }
                    | CompositorReply::Refreshed { .. }
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
