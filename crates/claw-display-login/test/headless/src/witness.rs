// SPDX-License-Identifier: GPL-3.0-only

use std::{
    fs::File,
    io::Read,
    time::{Duration, Instant},
};

use smithay::{
    input::Seat,
    wayland::selection::{SelectionSource, SelectionTarget, data_device, primary_selection},
};

use crate::state::State;

const MIME: &str = "text/plain;charset=utf-8";
const TAG: &str = "application/x-claw-private-write-witness";
const PAYLOAD: &[u8] = b"private-write-only-source";
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Transfer {
    file: File,
    bytes: Vec<u8>,
    deadline: Instant,
}

#[derive(Default)]
pub struct Witness {
    active: [bool; 2],
    pending: Vec<(SelectionTarget, bool)>,
    reads: Vec<Transfer>,
    sets: usize,
    payloads: usize,
    clears: usize,
}

impl Witness {
    pub fn changed(&mut self, target: SelectionTarget, source: Option<SelectionSource>) {
        let index = usize::from(target == SelectionTarget::Primary);
        let tagged = source
            .as_ref()
            .is_some_and(|source| source.mime_types().iter().any(|mime| mime == TAG));
        if tagged || (source.is_none() && self.active[index]) {
            assert!(
                self.pending.len() < 16,
                "private write witness queue exceeded"
            );
            self.pending.push((target, tagged));
            self.active[index] = tagged;
        }
    }

    pub fn dispatch(&mut self, seat: &Seat<State>) -> Result<()> {
        for (target, setting) in self.pending.drain(..) {
            let (reader, writer) = rustix::pipe::pipe_with(
                rustix::pipe::PipeFlags::CLOEXEC | rustix::pipe::PipeFlags::NONBLOCK,
            )?;
            let absent = match target {
                SelectionTarget::Clipboard => {
                    match data_device::request_data_device_client_selection(
                        seat,
                        MIME.to_string(),
                        writer,
                    ) {
                        Ok(()) => false,
                        Err(data_device::SelectionRequestError::NoSelection) => true,
                        Err(error) => return Err(error.into()),
                    }
                }
                SelectionTarget::Primary => {
                    match primary_selection::request_primary_client_selection(
                        seat,
                        MIME.to_string(),
                        writer,
                    ) {
                        Ok(()) => false,
                        Err(primary_selection::SelectionRequestError::NoSelection) => true,
                        Err(error) => return Err(error.into()),
                    }
                }
            };
            if setting == absent {
                return Err("private write-only state mutation did not take effect".into());
            }
            if setting {
                if self.reads.len() >= 16 {
                    return Err("private write witness transfer bound exceeded".into());
                }
                self.sets += 1;
                self.reads.push(Transfer {
                    file: reader.into(),
                    bytes: Vec::new(),
                    deadline: Instant::now() + Duration::from_secs(2),
                });
            } else {
                self.clears += 1;
            }
        }
        self.poll()
    }

    fn poll(&mut self) -> Result<()> {
        for mut transfer in std::mem::take(&mut self.reads) {
            let mut bytes = [0; 64];
            match transfer.file.read(&mut bytes) {
                Ok(0) => {
                    if transfer.bytes != PAYLOAD {
                        return Err(
                            "private write-only payload differs from the actual source".into()
                        );
                    }
                    self.payloads += 1;
                    continue;
                }
                Ok(count) => transfer.bytes.extend_from_slice(&bytes[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
            if transfer.bytes.len() > 64 || Instant::now() >= transfer.deadline {
                return Err("private write-only payload exceeded its bound or deadline".into());
            }
            self.reads.push(transfer);
        }
        Ok(())
    }

    pub fn finish(&mut self) -> Result<()> {
        while !self.reads.is_empty() {
            self.poll()?;
            std::thread::sleep(Duration::from_millis(1));
        }
        if !self.pending.is_empty()
            || self.active.iter().any(|active| *active)
            || self.sets != self.payloads
            || self.sets != self.clears
        {
            return Err("private write-only witness did not complete every mutation".into());
        }
        if self.sets != 0 {
            eprintln!(
                "private write-only witness: {} payloads, {} clears",
                self.payloads, self.clears,
            );
        }
        Ok(())
    }
}
