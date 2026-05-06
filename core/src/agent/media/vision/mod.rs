//! Vision subsystem: input routing + analysis.
//!
//! `routing` decides whether an image should go to a vision-capable
//! LLM as a native image input, or be OCR'd / summarised first
//! and presented as text. `analyze` (future) packages an image
//! and prompt for a vision-capable provider.

pub mod routing;
