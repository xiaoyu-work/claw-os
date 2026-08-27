// SPDX-License-Identifier: GPL-3.0-only

use std::any::TypeId;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Start {
    Apply { attempt: u64 },
    WriteMarker { attempt: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageResult {
    Ignored,
    Waiting,
    Failed,
    WriteMarker,
}

#[derive(Debug)]
pub struct Coordinator {
    next_attempt: u64,
    active_attempt: Option<u64>,
    pending_pages: HashSet<TypeId>,
    marker_ready: bool,
}

impl Coordinator {
    pub fn new(marker_ready: bool) -> Self {
        Self {
            next_attempt: 0,
            active_attempt: None,
            pending_pages: HashSet::new(),
            marker_ready,
        }
    }

    pub fn finishing(&self) -> bool {
        self.active_attempt.is_some()
    }

    pub fn begin(&mut self, pages: impl IntoIterator<Item = TypeId>) -> Option<Start> {
        if self.finishing() {
            return None;
        }

        self.next_attempt = self.next_attempt.wrapping_add(1);
        let attempt = self.next_attempt;
        self.active_attempt = Some(attempt);
        self.pending_pages = pages.into_iter().collect();

        if self.marker_ready || self.pending_pages.is_empty() {
            self.marker_ready = true;
            Some(Start::WriteMarker { attempt })
        } else {
            Some(Start::Apply { attempt })
        }
    }

    pub fn is_active(&self, attempt: u64) -> bool {
        self.active_attempt == Some(attempt)
    }

    pub fn is_pending(&self, attempt: u64, page: TypeId) -> bool {
        self.is_active(attempt) && self.pending_pages.contains(&page)
    }

    pub fn page_finished(&mut self, attempt: u64, page: TypeId, succeeded: bool) -> PageResult {
        if !self.is_active(attempt) || !self.pending_pages.remove(&page) {
            return PageResult::Ignored;
        }

        if !succeeded {
            self.active_attempt = None;
            self.pending_pages.clear();
            return PageResult::Failed;
        }

        if self.pending_pages.is_empty() {
            self.marker_ready = true;
            PageResult::WriteMarker
        } else {
            PageResult::Waiting
        }
    }

    pub fn operation_failed(&mut self, attempt: u64) -> bool {
        if !self.is_active(attempt) {
            return false;
        }

        self.active_attempt = None;
        self.pending_pages.clear();
        true
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/finish.rs"));
}
