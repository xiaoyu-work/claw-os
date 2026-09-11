// SPDX-License-Identifier: GPL-3.0-only

use std::{
    collections::{HashMap, HashSet},
    os::{fd::AsFd, unix::net::UnixStream},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use claw_display_control::{
    CompositorCommand, CompositorReply, Epoch, Error, InstanceId, ProcessIdentity, Received,
    compositor::{CompositorLink, DisplayBinding},
    wire::monotonic_ms,
    workload::InstanceProcess,
};
use smithay::reexports::{
    calloop::RegistrationToken,
    wayland_server::{
        Client, DisplayHandle,
        backend::{ClientData, DisconnectReason},
    },
};

const MAX_INSTANCES: usize = 64;
const MAX_EPOCH_INSTANCES: usize = 4096;
const MAX_LEASE_MS: u64 = 300_000;

pub struct InstanceLease {
    binding: Arc<DisplayBinding>,
    id: InstanceId,
    authority: Epoch,
    revision: u64,
    process: InstanceProcess,
    relay: ProcessIdentity,
    expires_ms: AtomicU64,
    retired: AtomicBool,
    context_used: AtomicBool,
    read: bool,
    write: bool,
    layer_shell: bool,
}

impl InstanceLease {
    fn active(&self) -> Result<bool, Error> {
        let active = !self.retired.load(Ordering::Acquire)
            && self.binding.active()?
            && monotonic_ms()? < self.expires_ms.load(Ordering::Acquire)
            && self.process.active()?;
        if active {
            self.relay.assert_current()?;
        }
        Ok(active)
    }
}

#[derive(Clone)]
pub enum ClientOrigin {
    Login(Arc<DisplayBinding>),
    Creator(Arc<InstanceLease>),
    Instance(Arc<InstanceLease>),
}

impl ClientOrigin {
    pub fn selection_read(&self) -> bool {
        self.selection(false)
    }

    pub fn selection_write(&self) -> bool {
        self.selection(true)
    }

    fn selection(&self, write: bool) -> bool {
        let Self::Instance(lease) = self else {
            return false;
        };
        match lease.active() {
            Ok(true) => {
                if write {
                    lease.write
                } else {
                    lease.read
                }
            }
            Ok(false) => false,
            Err(error) => {
                tracing::warn!(%error, "GUI selection identity check failed");
                false
            }
        }
    }

    pub fn layer_shell(&self) -> bool {
        match self {
            Self::Login(binding) => checked(binding.active()),
            Self::Instance(lease) => lease.layer_shell && checked(lease.active()),
            Self::Creator(_) => false,
        }
    }

    pub fn os_session(&self) -> bool {
        matches!(self, Self::Login(binding) if checked(binding.active()))
    }

    pub fn can_create_context(&self) -> bool {
        match self {
            Self::Login(binding) => checked(binding.active()),
            Self::Creator(lease) => {
                checked(lease.active()) && !lease.context_used.load(Ordering::Acquire)
            }
            Self::Instance(_) => false,
        }
    }

    pub fn context(&self) -> Result<Self, Error> {
        match self {
            Self::Login(binding) if binding.active()? => Ok(self.clone()),
            Self::Creator(lease) if lease.active()? => {
                if lease.context_used.swap(true, Ordering::AcqRel) {
                    return Err(Error::Protocol("GUI creator lease was already consumed"));
                }
                Ok(Self::Instance(lease.clone()))
            }
            _ => Err(Error::Protocol(
                "security context has no Root creator authority",
            )),
        }
    }

    pub fn accepts(&self, stream: &UnixStream) -> Result<bool, Error> {
        let peer = ProcessIdentity::unix_peer(stream.as_fd())?;
        match self {
            Self::Login(binding) => binding.is_login_peer(&peer),
            Self::Instance(lease) => Ok(lease.active()? && peer == lease.relay),
            Self::Creator(_) => Ok(false),
        }
    }

    pub fn instance(&self) -> Option<InstanceId> {
        match self {
            Self::Creator(lease) | Self::Instance(lease) => Some(lease.id),
            Self::Login(_) => None,
        }
    }
}

struct Instance {
    lease: Arc<InstanceLease>,
    clients: Vec<Client>,
    listener: Option<RegistrationToken>,
}

pub struct DisplayAuthority {
    link: CompositorLink,
    instances: HashMap<InstanceId, Instance>,
    used: HashSet<InstanceId>,
}

impl std::fmt::Debug for DisplayAuthority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DisplayAuthority")
            .field("epoch", &self.epoch())
            .field("instances", &self.instances.len())
            .finish_non_exhaustive()
    }
}

impl DisplayAuthority {
    pub fn receive() -> Result<Self, Error> {
        Ok(Self {
            link: CompositorLink::receive()?,
            instances: HashMap::new(),
            used: HashSet::new(),
        })
    }

    pub fn login_origin(&self) -> ClientOrigin {
        ClientOrigin::Login(self.link.binding())
    }

    pub fn ready(&self, name: String) -> Result<(), Error> {
        self.link.ready(name)
    }

    pub fn next(&self) -> Result<Option<Received<CompositorCommand>>, Error> {
        self.link.next()
    }

    pub fn creator(
        &mut self,
        dh: &DisplayHandle,
        mut packet: Received<CompositorCommand>,
        data: impl FnOnce(ClientOrigin) -> Arc<dyn ClientData>,
    ) -> Result<(), Error> {
        let CompositorCommand::Creator {
            epoch,
            owner_uid,
            instance,
            authority,
            revision,
            expires_monotonic_ms,
            selection_read,
            selection_write,
            layer_shell,
        } = packet.message
        else {
            return Err(Error::Protocol("expected GUI creator command"));
        };
        let binding = self.link.binding();
        validate_expiry(expires_monotonic_ms)?;
        if epoch != binding.epoch()
            || owner_uid != binding.owner_uid()
            || !binding.active()?
            || instance.0 == [0; 16]
            || authority.0 == [0; 16]
            || self.used.contains(&instance)
            || self.instances.len() >= MAX_INSTANCES
            || self.used.len() >= MAX_EPOCH_INSTANCES
        {
            return Err(Error::Protocol(
                "invalid, stale or duplicate GUI creator lease",
            ));
        }
        if packet.descriptors.len() != 3 {
            return Err(Error::Protocol(
                "GUI creator requires exactly three descriptors",
            ));
        }
        let group = packet.descriptors.pop().ok_or(Error::Identity)?;
        let pidfd = packet.descriptors.pop().ok_or(Error::Identity)?;
        if ProcessIdentity::from_pidfd(pidfd.as_fd())?.uid() != owner_uid {
            return Err(Error::Protocol("GUI worker belongs to another display owner"));
        }
        let process = InstanceProcess::receive(pidfd, group)?;
        let descriptor = packet.descriptors.pop().ok_or(Error::Identity)?;
        let stream = claw_display_control::identity::unix_stream(descriptor)?;
        let relay = ProcessIdentity::unix_peer(stream.as_fd())?;
        if relay.uid() != 0 {
            return Err(Error::Protocol("GUI creator stream was not made by Root"));
        }
        let lease = Arc::new(InstanceLease {
            binding,
            id: instance,
            authority,
            revision,
            process,
            relay,
            expires_ms: AtomicU64::new(expires_monotonic_ms),
            retired: AtomicBool::new(false),
            context_used: AtomicBool::new(false),
            read: selection_read,
            write: selection_write,
            layer_shell,
        });
        let origin = ClientOrigin::Creator(lease.clone());
        let client = dh.clone().insert_client(stream, data(origin))?;
        self.instances.insert(
            instance,
            Instance {
                lease,
                clients: vec![client],
                listener: None,
            },
        );
        self.used.insert(instance);
        self.link
            .reply(&CompositorReply::CreatorReady { epoch, instance })
    }

    pub fn track_client(&mut self, dh: &DisplayHandle, origin: &ClientOrigin, client: Client) -> Result<(), Error> {
        if let Some(id) = origin.instance() {
            let instance = self.instances.get_mut(&id).ok_or(Error::Identity)?;
            instance.clients.retain(|client| dh.backend_handle().get_client_data(client.id()).is_ok());
            if !instance.lease.active()? || instance.clients.len() >= 128 {
                return Err(Error::Protocol(
                    "GUI instance is retired or at connection capacity",
                ));
            }
            instance.clients.push(client);
        }
        Ok(())
    }

    pub fn track_listener(
        &mut self,
        origin: &ClientOrigin,
        token: RegistrationToken,
    ) -> Result<(), Error> {
        if let Some(id) = origin.instance() {
            let instance = self.instances.get_mut(&id).ok_or(Error::Identity)?;
            if instance.listener.is_some() || !instance.lease.active()? {
                return Err(Error::Protocol("GUI listener already exists or is retired"));
            }
            instance.listener = Some(token);
        }
        Ok(())
    }

    pub fn refresh(&mut self, message: CompositorCommand) -> Result<(), Error> {
        let CompositorCommand::Refresh {
            epoch,
            instance,
            authority,
            revision,
            expires_monotonic_ms,
        } = message
        else {
            return Err(Error::Protocol("expected GUI refresh"));
        };
        validate_expiry(expires_monotonic_ms)?;
        let entry = self.instances.get(&instance).ok_or(Error::Identity)?;
        if epoch != self.link.binding().epoch()
            || authority != entry.lease.authority
            || revision != entry.lease.revision
            || !entry.lease.active()?
        {
            return Err(Error::Protocol(
                "GUI refresh cannot alter or resurrect authority",
            ));
        }
        entry
            .lease
            .expires_ms
            .store(expires_monotonic_ms, Ordering::Release);
        self.link
            .reply(&CompositorReply::Refreshed { epoch, instance })
    }

    pub fn retire(
        &mut self,
        dh: &DisplayHandle,
        epoch: Epoch,
        id: InstanceId,
        remove: impl FnOnce(RegistrationToken),
    ) -> Result<(), Error> {
        self.remove_instance(dh, epoch, id, remove)?;
        self.link.reply(&CompositorReply::Retired {
            epoch,
            instance: id,
        })
    }

    fn remove_instance(
        &mut self,
        dh: &DisplayHandle,
        epoch: Epoch,
        id: InstanceId,
        remove: impl FnOnce(RegistrationToken),
    ) -> Result<(), Error> {
        if epoch != self.link.binding().epoch() {
            return Err(Error::Protocol("stale GUI retirement epoch"));
        }
        if let Some(instance) = self.instances.remove(&id) {
            instance.lease.retired.store(true, Ordering::Release);
            if let Some(listener) = instance.listener {
                remove(listener);
            }
            for client in instance.clients {
                dh.backend_handle()
                    .kill_client(client.id(), DisconnectReason::ConnectionClosed);
            }
        } else if !self.used.contains(&id) {
            if self.used.len() >= MAX_EPOCH_INSTANCES {
                return Err(Error::Protocol("GUI retirement tombstone capacity reached"));
            }
            // A lost creator reply must still be safely retireable. The tombstone
            // also prevents a delayed creation from resurrecting this instance.
            self.used.insert(id);
        }
        Ok(())
    }

    pub fn expired(&self) -> Vec<InstanceId> {
        self.instances
            .iter()
            .filter_map(|(id, entry)| (!checked(entry.lease.active())).then_some(*id))
            .collect()
    }

    pub fn epoch(&self) -> Epoch {
        self.link.binding().epoch()
    }

    fn reject(&self, instance: InstanceId, error: Error) -> Result<(), Error> {
        use claw_display_control::wire::GuiRefusal;
        let reason = match error {
            Error::Identity | Error::UnauthenticatedLogin | Error::Protocol(_) => GuiRefusal::InvalidBinding,
            Error::Io(_) | Error::Closed => GuiRefusal::Unavailable,
            Error::Timeout => GuiRefusal::Expired,
        };
        tracing::warn!(%error, "Root GUI control transition refused");
        self.link.reply(&CompositorReply::Rejected { epoch: self.epoch(), instance, reason })
    }

    pub fn stop(
        &mut self,
        dh: &DisplayHandle,
        mut remove: impl FnMut(RegistrationToken),
    ) -> Result<(), Error> {
        for (_, instance) in self.instances.drain() {
            instance.lease.retired.store(true, Ordering::Release);
            if let Some(listener) = instance.listener {
                remove(listener);
            }
            for client in instance.clients {
                dh.backend_handle()
                    .kill_client(client.id(), DisconnectReason::ConnectionClosed);
            }
        }
        self.link.reply(&CompositorReply::Stopped {
            epoch: self.epoch(),
        })
    }
}

fn checked(result: Result<bool, Error>) -> bool {
    match result {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, "display authority check failed");
            false
        }
    }
}

fn validate_expiry(expires: u64) -> Result<(), Error> {
    let now = monotonic_ms()?;
    if expires <= now {
        return Err(Error::Timeout);
    }
    if expires > now.saturating_add(MAX_LEASE_MS) {
        return Err(Error::Protocol("GUI lease expiry is invalid"));
    }
    Ok(())
}

pub fn dispatch(state: &mut crate::state::State) -> Result<(), Error> {
    let mut authority = state
        .common
        .display_authority
        .take()
        .ok_or(Error::Identity)?;
    let dh = state.common.display_handle.clone();
    let handle = state.common.event_loop_handle.clone();
    let result = (|| {
        for _ in 0..16 {
            let Some(packet) = authority.next()? else {
                break;
            };
            let instance = match &packet.message {
                CompositorCommand::Creator { instance, .. } | CompositorCommand::Refresh { instance, .. }
                    | CompositorCommand::Retire { instance, .. } => Some(*instance),
                _ => None,
            };
            let transition = match packet.message {
                CompositorCommand::Creator { .. } => {
                    authority.creator(&dh, packet, |origin| {
                        let mut data = state.new_client_state();
                        data.display_origin = Some(origin);
                        Arc::new(data)
                    })
                }
                CompositorCommand::Refresh { .. } => authority.refresh(packet.message),
                CompositorCommand::Retire { epoch, instance } => {
                    authority.retire(&dh, epoch, instance, |token| handle.remove(token))
                }
                CompositorCommand::Shutdown { epoch } if epoch == authority.epoch() => {
                    authority.stop(&dh, |token| handle.remove(token))?;
                    state.common.should_stop = true;
                    break;
                }
                _ => return Err(Error::Protocol("unexpected compositor control command")),
            };
            if let Err(error) = transition {
                authority.reject(instance.ok_or(Error::Identity)?, error)?;
            }
        }
        for instance in authority.expired() {
            authority.remove_instance(&dh, authority.epoch(), instance, |token| {
                handle.remove(token)
            })?;
            authority.link.reply(&CompositorReply::InstanceEnded {
                epoch: authority.epoch(),
                instance,
            })?;
        }
        Ok(())
    })();
    if result.is_err()
        && let Err(error) = authority.stop(&dh, |token| handle.remove(token))
    {
        tracing::error!(%error, "display authority termination could not notify Root");
    }
    state.common.display_authority = Some(authority);
    result
}
