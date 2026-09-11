//! Read-only attachment to the display created by the authenticated login.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::wire::{SessionEvent, SessionRequest, SESSION_DIRECTORY};
use crate::{Connection, Epoch, Error, KernelLogin, ProcessIdentity};

pub struct Subscription {
    connection: Connection,
    host: ProcessIdentity,
    owner_uid: u32,
    epoch: Epoch,
    display: String,
}

impl Subscription {
    pub fn connect() -> Result<Self, Error> {
        let current = ProcessIdentity::current()?;
        let login = KernelLogin::of(&current)?;
        if current.uid() != login.owner_uid {
            return Err(Error::Identity);
        }
        let path = Path::new(SESSION_DIRECTORY).join(format!("login-{}.sock", login.session_id));
        let connection = Connection::connect(&path)?;
        let host = connection.peer()?;
        if host.uid() != 0 || KernelLogin::of(&host)? != login {
            return Err(Error::Identity);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        connection.send(&SessionRequest::Subscribe {}, &[], deadline)?;
        let packet = connection.receive::<SessionEvent>(&host, deadline)?;
        let SessionEvent::Ready {
            epoch,
            wayland_display: display,
        } = packet.message
        else {
            return Err(Error::Protocol("authenticated display is unavailable"));
        };
        Ok(Self {
            connection,
            host,
            owner_uid: login.owner_uid,
            epoch,
            display,
        })
    }

    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    pub fn display(&self) -> &str {
        &self.display
    }

    pub fn runtime_dir(&self) -> String {
        format!("/run/user/{}", self.owner_uid)
    }

    pub fn bus_address(&self) -> String {
        format!("unix:path={}/bus", self.runtime_dir())
    }

    pub fn ended(&self, deadline: Instant) -> Result<bool, Error> {
        match self
            .connection
            .receive::<SessionEvent>(&self.host, deadline)
        {
            Ok(packet) => match packet.message {
                SessionEvent::Ended { epoch } if epoch == self.epoch => Ok(true),
                _ => Err(Error::Protocol("invalid display session lifecycle event")),
            },
            Err(Error::Timeout) => Ok(false),
            Err(error) => Err(error),
        }
    }
}
