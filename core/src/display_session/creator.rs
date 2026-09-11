use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Instant;

use claw_display_control::{CompositorCommand, CompositorReply, Epoch, Error, InstanceId};
use wayland_client::{
    protocol::{wl_callback, wl_registry},
    Connection, Dispatch, EventQueue, QueueHandle,
};
use wayland_protocols::wp::security_context::v1::client::{
    wp_security_context_manager_v1::WpSecurityContextManagerV1,
    wp_security_context_v1::WpSecurityContextV1,
};

use super::registry::GuiControl;

pub(crate) struct Creator {
    _close: UnixStream,
}

pub(crate) struct Lease {
    pub instance: InstanceId,
    pub authority: Epoch,
    pub revision: u64,
    pub expires_monotonic_ms: u64,
    pub selection_read: bool,
    pub selection_write: bool,
    pub layer_shell: bool,
}

impl Creator {
    pub fn install(
        control: &GuiControl,
        lease: Lease,
        process: OwnedFd,
        cgroup: OwnedFd,
        listener: &UnixListener,
        app_id: &str,
        deadline: Instant,
    ) -> Result<Self, Error> {
        let (server, client) = UnixStream::pair()?;
        let reply = control.exchange(
            CompositorCommand::Creator {
                epoch: control.epoch,
                owner_uid: control.owner_uid,
                instance: lease.instance,
                authority: lease.authority,
                revision: lease.revision,
                expires_monotonic_ms: lease.expires_monotonic_ms,
                selection_read: lease.selection_read,
                selection_write: lease.selection_write,
                layer_shell: lease.layer_shell,
            },
            vec![server.into(), process, cgroup],
            deadline,
        )?;
        match reply {
            CompositorReply::CreatorReady { epoch, instance }
                if epoch == control.epoch && instance == lease.instance => {}
            CompositorReply::Rejected {
                epoch,
                instance,
                reason,
            } if epoch == control.epoch && instance == lease.instance => {
                use claw_display_control::wire::GuiRefusal;
                return Err(Error::Protocol(match reason {
                    GuiRefusal::InvalidBinding => "compositor refused the GUI creator binding",
                    GuiRefusal::Unavailable => "compositor GUI creator resource is unavailable",
                    GuiRefusal::Expired => "compositor GUI creator authority expired",
                }));
            }
            _ => {
                return Err(Error::Protocol(
                    "compositor did not install the GUI creator",
                ))
            }
        }
        let connection = Connection::from_socket(client)
            .map_err(|_| Error::Protocol("open private creator transport"))?;
        let mut queue = connection.new_event_queue::<State>();
        let handle = queue.handle();
        let mut state = State::default();
        let _registry = connection.display().get_registry(&handle, ());
        roundtrip(&connection, &mut queue, &mut state, deadline)?;
        let manager = state.manager.take().ok_or(Error::Protocol(
            "Root creator context global is unavailable",
        ))?;
        let (close, close_peer) = UnixStream::pair()?;
        let context = manager.create_listener(listener.as_fd(), close_peer.as_fd(), &handle, ());
        context.set_sandbox_engine("claw-os".to_string());
        context.set_app_id(app_id.to_string());
        context.set_instance_id(Epoch(lease.instance.0).label());
        context.commit();
        roundtrip(&connection, &mut queue, &mut state, deadline)?;
        Ok(Self { _close: close })
    }
}

#[derive(Default)]
struct State {
    manager: Option<WpSecurityContextManagerV1>,
    synced: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        handle: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            if interface == "wp_security_context_manager_v1" && version >= 1 {
                state.manager = Some(registry.bind(name, 1, handle, ()));
            }
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.synced = true;
    }
}

wayland_client::delegate_noop!(State: ignore WpSecurityContextManagerV1);
wayland_client::delegate_noop!(State: ignore WpSecurityContextV1);

fn roundtrip(
    connection: &Connection,
    queue: &mut EventQueue<State>,
    state: &mut State,
    deadline: Instant,
) -> Result<(), Error> {
    state.synced = false;
    connection.display().sync(&queue.handle(), ());
    while !state.synced {
        if Instant::now() >= deadline {
            return Err(Error::Timeout);
        }
        connection
            .flush()
            .map_err(|_| Error::Protocol("flush private creator requests"))?;
        queue
            .dispatch_pending(state)
            .map_err(|_| Error::Protocol("dispatch private creator events"))?;
        if state.synced {
            break;
        }
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let mut poll = libc::pollfd {
            fd: connection.as_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis();
        let result = unsafe { libc::poll(&mut poll, 1, remaining.min(i32::MAX as u128) as i32) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if result == 0 {
            return Err(Error::Timeout);
        }
        guard
            .read()
            .map_err(|_| Error::Protocol("read private creator events"))?;
    }
    Ok(())
}
