//! Reading and verifying a chain.
//!
//! Verification is a whole-chain property, not a per-line one: each
//! record must carry the next sequence, chain from the previous MAC, be
//! signed under a key this daemon holds, and end exactly where the
//! committed head says it should. Segment boundaries are part of that
//! chain — the first record of segment *n* must chain from the last
//! record of segment *n-1* — so rotation cannot be used to hide a gap.
//!
//! A read reports one of a small set of named health states rather than
//! "some lines parsed", and it never repairs. Repair is the writer's job
//! under the partition lock, and only for bytes past the committed head;
//! a reader that found damage says so, so diagnostics stay available
//! while mutations fail closed.

use std::path::Path;

use super::keyring::Keyring;
use super::partition::{Anchor, Partition};
use super::record::{JournalRecord, GENESIS_MAC};
use super::writer::load_anchor;
use super::JournalError;

/// What a verification pass concluded about one partition.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum Health {
    /// Chain, head and byte length all agree.
    Verified { events: u64, head_seq: u64 },
    /// The active segment holds bytes past the committed head. They were
    /// never acknowledged, so they are evidence of a crash, not of
    /// tampering.
    UncommittedTail { head_seq: u64, extra_bytes: u64 },
    /// The chain does not verify. Mutations on this partition must fail
    /// closed until an operator resolves it.
    Damaged { detail: String },
}

impl Health {
    pub fn is_verified(&self) -> bool {
        matches!(self, Self::Verified { .. })
    }

    pub fn is_damaged(&self) -> bool {
        matches!(self, Self::Damaged { .. })
    }
}

/// One partition's verified records, plus what verification concluded.
#[derive(Clone, Debug)]
pub struct Chain {
    pub partition: Partition,
    pub anchor: Anchor,
    pub records: Vec<JournalRecord>,
    pub health: Health,
}

/// Read and verify a partition across every retained segment.
///
/// `owner_uid` is only used to shape an empty anchor when the partition
/// has never been written; a partition that exists carries its own.
pub fn read(
    root: &Path,
    partition: &Partition,
    owner_uid: u32,
    keyring: &Keyring,
) -> Result<Chain, JournalError> {
    let anchor = match load_anchor(root, partition, owner_uid, keyring) {
        Ok(anchor) => anchor,
        Err(error @ (JournalError::Integrity(_) | JournalError::AnchorMissing { .. })) => {
            return Ok(Chain {
                partition: partition.clone(),
                anchor: Anchor::empty(partition, owner_uid, keyring.active_id()),
                records: Vec::new(),
                health: Health::Damaged {
                    detail: error.to_string(),
                },
            });
        }
        Err(error) => return Err(error),
    };

    let mut state = Walk::new(&anchor);
    let indexes = partition.segments(root)?;
    for index in indexes.iter().copied() {
        if index > anchor.active_index {
            state.damage(format!(
                "journal partition {partition} holds segment {index} beyond the active segment {}",
                anchor.active_index
            ));
            break;
        }
        let path = partition.segment_path(root, index);
        let data = match std::fs::read_to_string(&path) {
            Ok(data) => data,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(JournalError::io(&path, error)),
        };
        let is_active = index == anchor.active_index;
        let ceiling = if is_active {
            anchor.active_bytes
        } else {
            data.len() as u64
        };
        if !state.consume(&data, ceiling, is_active, keyring, &anchor) {
            break;
        }
    }

    let health = state.finish(root, partition, &anchor)?;
    Ok(Chain {
        partition: partition.clone(),
        anchor,
        records: state.records,
        health,
    })
}

/// Incremental verification state as segments are walked in order.
struct Walk {
    records: Vec<JournalRecord>,
    expected_seq: u64,
    prev: String,
    active_consumed: u64,
    damaged: Option<String>,
}

impl Walk {
    fn new(anchor: &Anchor) -> Self {
        let expected_seq = if anchor.first_seq == 0 {
            1
        } else {
            anchor.first_seq
        };
        let prev = if anchor.first_seq <= 1 {
            GENESIS_MAC.to_string()
        } else {
            anchor.first_prev_mac.clone()
        };
        Self {
            records: Vec::new(),
            expected_seq,
            prev,
            active_consumed: 0,
            damaged: None,
        }
    }

    fn damage(&mut self, detail: String) {
        if self.damaged.is_none() {
            self.damaged = Some(detail);
        }
    }

    /// Verify one segment's committed prefix. Returns `false` once the
    /// walk has found damage and should stop.
    fn consume(
        &mut self,
        data: &str,
        ceiling: u64,
        is_active: bool,
        keyring: &Keyring,
        anchor: &Anchor,
    ) -> bool {
        let mut consumed: u64 = 0;
        for line in data.split_inclusive('\n') {
            if consumed >= ceiling {
                break;
            }
            let trimmed = line.strip_suffix('\n').unwrap_or(line);
            if trimmed.trim().is_empty() {
                self.damage("journal chain holds an empty line".to_string());
                return false;
            }
            let record = match JournalRecord::decode_line(trimmed) {
                Ok(record) => record,
                Err(error) => {
                    self.damage(error.to_string());
                    return false;
                }
            };
            if record.partition != anchor.partition {
                self.damage(format!(
                    "journal record at seq {} names partition {}",
                    record.seq, record.partition
                ));
                return false;
            }
            if record.seq != self.expected_seq {
                self.damage(format!(
                    "journal chain expected seq {} and found {}",
                    self.expected_seq, record.seq
                ));
                return false;
            }
            if record.prev != self.prev {
                self.damage(format!(
                    "journal record at seq {} does not chain from the previous MAC",
                    record.seq
                ));
                return false;
            }
            let Some(key) = keyring.verify_key(&record.key_id) else {
                self.damage(format!(
                    "journal record at seq {} is signed with key {}, which this daemon does not \
                     hold",
                    record.seq, record.key_id
                ));
                return false;
            };
            if let Err(error) = record.verify(key) {
                self.damage(error.to_string());
                return false;
            }
            consumed += line.len() as u64;
            self.expected_seq = record.seq.saturating_add(1);
            self.prev = record.mac.clone();
            self.records.push(record);
        }
        if consumed != ceiling {
            self.damage(format!(
                "journal segment verified {consumed} byte(s); the committed head names {ceiling}"
            ));
            return false;
        }
        if is_active {
            self.active_consumed = consumed;
        }
        true
    }

    fn finish(
        &self,
        root: &Path,
        partition: &Partition,
        anchor: &Anchor,
    ) -> Result<Health, JournalError> {
        if let Some(detail) = &self.damaged {
            return Ok(Health::Damaged {
                detail: detail.clone(),
            });
        }
        if self.active_consumed != anchor.active_bytes {
            return Ok(Health::Damaged {
                detail: format!(
                    "journal active segment verified {} byte(s); the committed head names {}",
                    self.active_consumed, anchor.active_bytes
                ),
            });
        }
        let head = self
            .records
            .last()
            .map(|record| record.mac.as_str())
            .unwrap_or(self.prev.as_str());
        if head != anchor.head_mac {
            return Ok(Health::Damaged {
                detail: "journal chain head does not match the committed anchor".to_string(),
            });
        }
        let active_path = anchor.active_path(root, partition);
        let length = std::fs::metadata(&active_path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        if length > anchor.active_bytes {
            return Ok(Health::UncommittedTail {
                head_seq: anchor.seq,
                extra_bytes: length - anchor.active_bytes,
            });
        }
        if length < anchor.active_bytes {
            return Ok(Health::Damaged {
                detail: format!(
                    "journal active segment holds {length} byte(s); the committed head names {}",
                    anchor.active_bytes
                ),
            });
        }
        Ok(Health::Verified {
            events: self.records.len() as u64,
            head_seq: anchor.seq,
        })
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/session/journal/reader.rs"
    ));
}
