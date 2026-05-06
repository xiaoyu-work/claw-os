//! Local LLM task — routes to `engines::llama` (llama.cpp / GGUF).
//!
//! Surface mirrors `crate::agent::llm::Provider` so a `providers::local`
//! adapter can expose the in-process LLM as a regular agent provider.
