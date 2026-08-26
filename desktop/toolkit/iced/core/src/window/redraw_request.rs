use crate::time::Instant;

/// A request to redraw a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RedrawRequest {
    /// Redraw the next frame.
    NextFrame,

    /// Redraw at the given time.
    At(Instant),

    /// No redraw is needed.
    Wait,
}

impl From<Instant> for RedrawRequest {
    fn from(time: Instant) -> Self {
        Self::At(time)
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/window/redraw_request.rs"
    ));
}
