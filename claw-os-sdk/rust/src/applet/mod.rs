//! Versioned App data services, available through the same authenticated context
//! and resource scopes for every App. Language, UI and package origin confer no
//! authority. Providers remain OS-owned; this client opens no desktop transport
//! and never invokes another App.

mod client;
pub mod protocol;

pub use client::{Client, ClientError};
pub use protocol::{CalendarDate, CalendarEvent, HistoryPermission, SystemSummary, Task, Usage};
