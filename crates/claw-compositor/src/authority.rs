// SPDX-License-Identifier: GPL-3.0-only

use std::collections::{HashMap, HashSet};
use std::os::fd::{AsFd, BorrowedFd, IntoRawFd, OwnedFd};
use std::sync::Arc;

use claw_display_control::compositor::{CompositorLink, DisplayBinding};
use claw_display_control::selection::Payload;
use claw_display_control::wire::{monotonic_ms, GuiRefusal};
use claw_display_control::workload::InstanceProcess;
use claw_display_control::{
    CompositorCommand, CompositorReply, Epoch, Error, InstanceId, ProcessIdentity, Received,
};

const MAX_INSTANCES: usize = 64;
const MAX_EPOCH_INSTANCES: usize = 4096;
const MAX_CLIENTS: usize = 128;
const MAX_LEASE_MS: u64 = 300_000;

pub(crate) const READ: u32 = 1;
pub(crate) const WRITE: u32 = 2;
pub(crate) const PANEL: u32 = 4;
pub(crate) const CONTEXT: u32 = 8;
pub(crate) const OS_SESSION: u32 = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Origin {
    Login,
    Creator(u64),
    Instance(u64),
}

impl Origin {
    pub fn from_native(kind: u32, token: u64) -> Result<Self, Error> {
        match (kind, token) {
            (1, 0) => Ok(Self::Login),
            (2, token) if token != 0 => Ok(Self::Creator(token)),
            (3, token) if token != 0 => Ok(Self::Instance(token)),
            _ => Err(Error::Protocol("invalid native client origin")),
        }
    }

    fn token(self) -> Option<u64> {
        match self {
            Self::Login => None,
            Self::Creator(token) | Self::Instance(token) => Some(token),
        }
    }
}

#[derive(Default)]
struct GrantState {
    read: bool,
    write: bool,
    panel: bool,
    context_used: bool,
    retired: bool,
    clients: usize,
    listener: bool,
}

impl GrantState {
    fn permissions(&self, origin: Origin, active: bool) -> u32 {
        if !active || self.retired {
            return 0;
        }
        match origin {
            Origin::Login => OS_SESSION | PANEL | CONTEXT,
            Origin::Creator(_) => {
                if self.context_used {
                    0
                } else {
                    CONTEXT
                }
            }
            Origin::Instance(_) => {
                (u32::from(self.read) * READ)
                    | (u32::from(self.write) * WRITE)
                    | (u32::from(self.panel) * PANEL)
            }
        }
    }

    fn consume_context(&mut self, active: bool) -> Result<(), Error> {
        if !active || self.retired || self.context_used {
            return Err(Error::Protocol(
                "GUI creator context is unavailable or already used",
            ));
        }
        self.context_used = true;
        Ok(())
    }

    fn add_client(&mut self, active: bool) -> Result<(), Error> {
        if !active || self.retired || self.clients >= MAX_CLIENTS {
            return Err(Error::Protocol(
                "GUI client capacity or authority unavailable",
            ));
        }
        self.clients += 1;
        Ok(())
    }

    fn remove_client(&mut self) -> Result<(), Error> {
        self.clients = self
            .clients
            .checked_sub(1)
            .ok_or(Error::Protocol("GUI client retirement was repeated"))?;
        Ok(())
    }

    fn add_listener(&mut self, active: bool) -> Result<(), Error> {
        if !active || self.retired || !self.context_used || self.listener {
            return Err(Error::Protocol(
                "GUI listener authority is unavailable or already used",
            ));
        }
        self.listener = true;
        Ok(())
    }

    fn empty(&self) -> bool {
        self.clients == 0 && !self.listener
    }
}

struct Lease {
    binding: Arc<DisplayBinding>,
    instance: InstanceId,
    authority: Epoch,
    revision: u64,
    process: InstanceProcess,
    relay: ProcessIdentity,
    expires_ms: u64,
    state: GrantState,
}

impl Lease {
    fn active(&self) -> Result<bool, Error> {
        let active = !self.state.retired
            && self.binding.active()?
            && monotonic_ms()? < self.expires_ms
            && self.process.active()?;
        if active {
            self.relay.assert_current()?;
        }
        Ok(active)
    }

    fn accepts(&self, descriptor: BorrowedFd<'_>) -> Result<bool, Error> {
        Ok(self.active()? && ProcessIdentity::unix_peer(descriptor)? == self.relay)
    }
}

#[derive(Clone, Copy)]
#[repr(u32)]
pub(crate) enum CommandKind {
    Creator = 1,
    Retire = 2,
    Selection = 3,
    Stop = 4,
    Ended = 5,
}

#[derive(Clone, Copy)]
pub(crate) struct Command {
    pub serial: u64,
    pub token: u64,
    pub kind: CommandKind,
}

struct Pending {
    command: Command,
    instance: Option<InstanceId>,
    descriptor: Option<OwnedFd>,
    selection: Option<PendingSelection>,
}

struct PendingSelection {
    payload: Arc<Payload>,
    expires_ms: u64,
}

impl PendingSelection {
    fn validate(&self, active: bool, now: u64) -> Result<(), Error> {
        if !active {
            return Err(Error::Closed);
        }
        validate_expiry(self.expires_ms, now, 2000)
    }
}

pub(crate) struct Authority {
    link: CompositorLink,
    leases: HashMap<u64, Lease>,
    instances: HashMap<InstanceId, u64>,
    used: HashSet<InstanceId>,
    next_token: u64,
    next_serial: u64,
    pending: Option<Pending>,
    ready: bool,
}

impl Authority {
    pub fn open() -> Result<Self, Error> {
        Ok(Self {
            link: CompositorLink::receive()?,
            leases: HashMap::new(),
            instances: HashMap::new(),
            used: HashSet::new(),
            next_token: 1,
            next_serial: 1,
            pending: None,
            ready: false,
        })
    }

    pub fn ready(&mut self, display: String) -> Result<(), Error> {
        if self.ready {
            return Err(Error::Protocol("compositor readiness was repeated"));
        }
        self.link.ready(display)?;
        self.ready = true;
        Ok(())
    }

    pub fn rights(&self, origin: Origin) -> Result<u32, Error> {
        match origin.token() {
            None => Ok(GrantState::default().permissions(origin, self.link.binding().active()?)),
            Some(token) => {
                let lease = self.lease(token)?;
                Ok(lease.state.permissions(origin, lease.active()?))
            }
        }
    }

    pub fn accepts_login(&self, descriptor: BorrowedFd<'_>) -> Result<bool, Error> {
        self.link
            .binding()
            .is_login_peer(&ProcessIdentity::unix_peer(descriptor)?)
    }

    pub fn client_added(
        &mut self,
        origin: Origin,
        descriptor: BorrowedFd<'_>,
    ) -> Result<(), Error> {
        let Some(token) = origin.token() else {
            return if self.accepts_login(descriptor)? {
                Ok(())
            } else {
                Err(Error::Identity)
            };
        };
        if !self.lease(token)?.accepts(descriptor)? {
            return Err(Error::Identity);
        }
        let lease = self.lease_mut(token)?;
        lease.state.add_client(lease.active()?)
    }

    pub fn client_removed(&mut self, origin: Origin) -> Result<(), Error> {
        if let Some(token) = origin.token() {
            self.lease_mut(token)?.state.remove_client()?;
        }
        Ok(())
    }

    pub fn context_created(&mut self, origin: Origin) -> Result<(), Error> {
        match origin {
            Origin::Login if self.link.binding().active()? => Ok(()),
            Origin::Creator(token) => {
                let lease = self.lease_mut(token)?;
                lease.state.consume_context(lease.active()?)
            }
            _ => Err(Error::Protocol(
                "client has no Root context creator authority",
            )),
        }
    }

    pub fn listener_added(&mut self, token: u64) -> Result<(), Error> {
        if token == 0 {
            return if self.link.binding().active()? {
                Ok(())
            } else {
                Err(Error::Closed)
            };
        }
        let lease = self.lease_mut(token)?;
        lease.state.add_listener(lease.active()?)
    }

    pub fn listener_removed(&mut self, token: u64) -> Result<(), Error> {
        if token != 0 {
            let lease = self.lease_mut(token)?;
            if !lease.state.listener {
                return Err(Error::Protocol("GUI listener retirement was repeated"));
            }
            lease.state.listener = false;
        }
        Ok(())
    }

    pub fn accepts_context(&self, token: u64, descriptor: BorrowedFd<'_>) -> Result<bool, Error> {
        if token == 0 {
            self.accepts_login(descriptor)
        } else {
            self.lease(token)?.accepts(descriptor)
        }
    }

    pub fn next(&mut self) -> Result<Option<Command>, Error> {
        if self.pending.is_some() {
            return Err(Error::Protocol(
                "native compositor command was not acknowledged",
            ));
        }
        for _ in 0..16 {
            let Some(packet) = self.link.next()? else {
                break;
            };
            let instance = match packet.message {
                CompositorCommand::Creator { instance, .. }
                | CompositorCommand::Refresh { instance, .. }
                | CompositorCommand::Retire { instance, .. }
                | CompositorCommand::SetSelection { instance, .. } => Some(instance),
                _ => None,
            };
            match self.prepare(packet) {
                Ok(Some(command)) => return Ok(Some(command)),
                Ok(None) => {}
                Err(error) => {
                    let Some(instance) = instance else {
                        return Err(error);
                    };
                    self.reject(instance, error)?;
                }
            }
        }
        let expired = self
            .leases
            .iter()
            .find_map(|(&token, lease)| match lease.active() {
                Ok(true) => None,
                Ok(false) => Some(token),
                Err(error) => {
                    eprintln!("compositor GUI identity expired: {error}");
                    Some(token)
                }
            });
        if let Some(token) = expired {
            let instance = self.lease(token)?.instance;
            self.lease_mut(token)?.state.retired = true;
            return self
                .pending(CommandKind::Ended, token, Some(instance), None, None)
                .map(Some);
        }
        Ok(None)
    }

    fn prepare(
        &mut self,
        mut packet: Received<CompositorCommand>,
    ) -> Result<Option<Command>, Error> {
        let binding = self.link.binding();
        match packet.message {
            CompositorCommand::Creator {
                epoch,
                owner_uid,
                instance,
                authority,
                revision,
                expires_monotonic_ms,
                selection_read,
                selection_write,
                layer_shell,
            } => {
                validate_expiry(expires_monotonic_ms, monotonic_ms()?, MAX_LEASE_MS)?;
                if epoch != binding.epoch()
                    || owner_uid != binding.owner_uid()
                    || !binding.active()?
                    || instance.0 == [0; 16]
                    || authority.0 == [0; 16]
                    || self.used.contains(&instance)
                    || self.leases.len() >= MAX_INSTANCES
                    || self.used.len() >= MAX_EPOCH_INSTANCES
                    || packet.descriptors.len() != 3
                {
                    return Err(Error::Protocol("invalid or duplicate Root GUI creator"));
                }
                let group = packet.descriptors.pop().ok_or(Error::Identity)?;
                let pidfd = packet.descriptors.pop().ok_or(Error::Identity)?;
                if ProcessIdentity::from_pidfd(pidfd.as_fd())?.uid() != owner_uid {
                    return Err(Error::Identity);
                }
                let process = InstanceProcess::receive(pidfd, group)?;
                let descriptor = packet.descriptors.pop().ok_or(Error::Identity)?;
                let stream = claw_display_control::identity::unix_stream(descriptor)?;
                let relay = ProcessIdentity::unix_peer(stream.as_fd())?;
                if relay.uid() != 0 {
                    return Err(Error::Protocol("GUI creator stream was not made by Root"));
                }
                let token = sequence(&mut self.next_token)?;
                let command = self.pending(
                    CommandKind::Creator,
                    token,
                    Some(instance),
                    Some(stream.into()),
                    None,
                )?;
                self.leases.insert(
                    token,
                    Lease {
                        binding,
                        instance,
                        authority,
                        revision,
                        process,
                        relay,
                        expires_ms: expires_monotonic_ms,
                        state: GrantState {
                            read: selection_read,
                            write: selection_write,
                            panel: layer_shell,
                            ..GrantState::default()
                        },
                    },
                );
                self.instances.insert(instance, token);
                self.used.insert(instance);
                Ok(Some(command))
            }
            CompositorCommand::Refresh {
                epoch,
                instance,
                authority,
                revision,
                expires_monotonic_ms,
            } => {
                validate_expiry(expires_monotonic_ms, monotonic_ms()?, MAX_LEASE_MS)?;
                let token = *self.instances.get(&instance).ok_or(Error::Identity)?;
                let lease = self.lease_mut(token)?;
                if epoch != binding.epoch()
                    || authority != lease.authority
                    || revision != lease.revision
                    || !lease.active()?
                {
                    return Err(Error::Protocol(
                        "GUI renewal cannot change or revive authority",
                    ));
                }
                lease.expires_ms = expires_monotonic_ms;
                self.link
                    .reply(&CompositorReply::Refreshed { epoch, instance })?;
                Ok(None)
            }
            CompositorCommand::Retire { epoch, instance } => {
                if epoch != binding.epoch() {
                    return Err(Error::Protocol("stale GUI retirement epoch"));
                }
                let token = self.instances.get(&instance).copied().unwrap_or(0);
                if token == 0 && !self.used.contains(&instance) {
                    if self.used.len() >= MAX_EPOCH_INSTANCES {
                        return Err(Error::Protocol("GUI retirement tombstone capacity reached"));
                    }
                    self.used.insert(instance);
                }
                if token != 0 {
                    self.lease_mut(token)?.state.retired = true;
                }
                self.pending(CommandKind::Retire, token, Some(instance), None, None)
                    .map(Some)
            }
            CompositorCommand::SetSelection {
                epoch,
                owner_uid,
                instance,
                expires_monotonic_ms,
            } => {
                if epoch != binding.epoch()
                    || owner_uid != binding.owner_uid()
                    || !binding.active()?
                {
                    return Err(Error::Identity);
                }
                validate_expiry(expires_monotonic_ms, monotonic_ms()?, 2000)?;
                let descriptor = packet.descriptors.pop().ok_or(Error::Identity)?;
                let payload = Payload::receive(descriptor)?;
                validate_expiry(expires_monotonic_ms, monotonic_ms()?, 2000)?;
                self.pending(
                    CommandKind::Selection,
                    0,
                    Some(instance),
                    None,
                    Some(PendingSelection {
                        payload: Arc::new(payload),
                        expires_ms: expires_monotonic_ms,
                    }),
                )
                .map(Some)
            }
            CompositorCommand::Shutdown { epoch } if epoch == binding.epoch() => {
                for lease in self.leases.values_mut() {
                    lease.state.retired = true;
                }
                self.pending(CommandKind::Stop, 0, None, None, None)
                    .map(Some)
            }
            _ => Err(Error::Protocol("unexpected compositor control command")),
        }
    }

    pub fn take_descriptor(&mut self, serial: u64) -> Result<i32, Error> {
        let token = self
            .pending
            .as_ref()
            .filter(|pending| pending.command.serial == serial)
            .ok_or(Error::Identity)?
            .command
            .token;
        if !self.lease(token)?.active()? {
            return Err(Error::Closed);
        }
        self.pending
            .as_mut()
            .filter(|pending| pending.command.serial == serial)
            .and_then(|pending| pending.descriptor.take())
            .map(IntoRawFd::into_raw_fd)
            .ok_or(Error::Protocol(
                "native creator descriptor is unavailable or already taken",
            ))
    }

    pub fn take_selection(&mut self, serial: u64) -> Result<Arc<Payload>, Error> {
        let active = self.link.binding().active()?;
        let pending = self
            .pending
            .as_mut()
            .filter(|pending| pending.command.serial == serial)
            .ok_or(Error::Identity)?;
        let selection = pending.selection.as_ref().ok_or(Error::Protocol(
            "native selection payload is unavailable or already taken",
        ))?;
        selection.validate(active, monotonic_ms()?)?;
        Ok(pending.selection.take().ok_or(Error::Identity)?.payload)
    }

    pub fn acknowledge(&mut self, serial: u64, accepted: bool) -> Result<(), Error> {
        if self.pending.as_ref().map(|pending| pending.command.serial) != Some(serial) {
            return Err(Error::Protocol(
                "native acknowledgement does not match pending command",
            ));
        }
        let pending = self.pending.take().ok_or(Error::Identity)?;
        let epoch = self.link.binding().epoch();
        let token = pending.command.token;
        let instance = pending.instance;
        match pending.command.kind {
            CommandKind::Creator => {
                let instance = instance.ok_or(Error::Identity)?;
                let lease = self.lease(token)?;
                if accepted && pending.descriptor.is_none() && lease.state.clients > 0 {
                    self.link
                        .reply(&CompositorReply::CreatorReady { epoch, instance })
                } else {
                    self.require_empty(token)?;
                    self.remove(token);
                    self.reject(
                        instance,
                        Error::Protocol("native creator installation failed"),
                    )
                }
            }
            CommandKind::Retire | CommandKind::Ended => {
                if !accepted {
                    return Err(Error::Protocol("native GUI retirement was not confirmed"));
                }
                self.require_empty(token)?;
                self.remove(token);
                let instance = instance.ok_or(Error::Identity)?;
                self.link.reply(&match pending.command.kind {
                    CommandKind::Retire => CompositorReply::Retired { epoch, instance },
                    _ => CompositorReply::InstanceEnded { epoch, instance },
                })
            }
            CommandKind::Selection => {
                let instance = instance.ok_or(Error::Identity)?;
                if accepted && pending.selection.is_none() {
                    self.link
                        .reply(&CompositorReply::SelectionSet { epoch, instance })
                } else {
                    self.reject(
                        instance,
                        Error::Protocol("native selection installation failed"),
                    )
                }
            }
            CommandKind::Stop => {
                if !accepted || self.leases.values().any(|lease| !lease.state.empty()) {
                    return Err(Error::Protocol(
                        "native display retirement was not confirmed",
                    ));
                }
                self.leases.clear();
                self.instances.clear();
                self.link.reply(&CompositorReply::Stopped { epoch })
            }
        }
    }

    fn pending(
        &mut self,
        kind: CommandKind,
        token: u64,
        instance: Option<InstanceId>,
        descriptor: Option<OwnedFd>,
        selection: Option<PendingSelection>,
    ) -> Result<Command, Error> {
        let command = Command {
            serial: sequence(&mut self.next_serial)?,
            token,
            kind,
        };
        self.pending = Some(Pending {
            command,
            instance,
            descriptor,
            selection,
        });
        Ok(command)
    }

    fn lease(&self, token: u64) -> Result<&Lease, Error> {
        self.leases.get(&token).ok_or(Error::Identity)
    }

    fn lease_mut(&mut self, token: u64) -> Result<&mut Lease, Error> {
        self.leases.get_mut(&token).ok_or(Error::Identity)
    }

    fn require_empty(&self, token: u64) -> Result<(), Error> {
        if token != 0 && !self.lease(token)?.state.empty() {
            return Err(Error::Protocol(
                "GUI resources are still owned by the native compositor",
            ));
        }
        Ok(())
    }

    fn remove(&mut self, token: u64) {
        if let Some(lease) = self.leases.remove(&token) {
            self.instances.remove(&lease.instance);
        }
    }

    fn reject(&self, instance: InstanceId, error: Error) -> Result<(), Error> {
        let reason = match error {
            Error::Io(_) | Error::Closed => GuiRefusal::Unavailable,
            Error::Timeout => GuiRefusal::Expired,
            _ => GuiRefusal::InvalidBinding,
        };
        eprintln!("compositor GUI control refused: {error}");
        self.link.reply(&CompositorReply::Rejected {
            epoch: self.link.binding().epoch(),
            instance,
            reason,
        })
    }
}

fn sequence(next: &mut u64) -> Result<u64, Error> {
    let value = *next;
    *next = next
        .checked_add(1)
        .ok_or(Error::Protocol("native compositor sequence exhausted"))?;
    Ok(value)
}

fn validate_expiry(expires: u64, now: u64, maximum: u64) -> Result<(), Error> {
    if expires <= now {
        Err(Error::Timeout)
    } else if expires > now.saturating_add(maximum) {
        Err(Error::Protocol("GUI lease expiry exceeds its bound"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/authority.rs"
    ));
}
