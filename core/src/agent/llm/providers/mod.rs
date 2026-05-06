//! LLM provider implementations.
//!
//! Phase 1 ships only the `mock` provider so the runtime can be tested
//! end-to-end without any external API. Real providers (top-9 from Q3:
//! anthropic, openai, gemini, openrouter, ollama, bedrock, custom, xai,
//! deepseek) plus the `local` (llama.cpp) adapter land in later phases
//! when a concrete one is selected.

pub mod mock;
