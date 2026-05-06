//! Tool registry — collection of `Arc<dyn Tool>` keyed by name.

use std::collections::HashMap;
use std::sync::Arc;

use super::Tool;
use crate::agent::llm;

#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<&'static str, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool. Last write wins for duplicate names.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name(), tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn names(&self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = self.tools.keys().copied().collect();
        names.sort_unstable();
        names
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Convert to the LLM-trait-facing representation passed in `ChatRequest.tools`.
    pub fn as_llm_tools(&self) -> Vec<llm::Tool> {
        let mut out: Vec<llm::Tool> = self
            .tools
            .values()
            .map(|t| llm::Tool {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

/// Build the default registry shipped with `cos agent`. Phase 1 includes
/// only side-effect-free tools.
pub fn default_registry() -> ToolRegistry {
    let mut r = ToolRegistry::new();
    r.register(Arc::new(super::builtin::Echo));
    r.register(Arc::new(super::builtin::Now));
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_registry_has_builtins() {
        let r = default_registry();
        assert!(r.get("echo").is_some());
        assert!(r.get("now").is_some());
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn names_are_sorted() {
        let r = default_registry();
        let names = r.names();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn as_llm_tools_round_trips_schema() {
        let r = default_registry();
        let tools = r.as_llm_tools();
        assert!(tools.iter().any(|t| t.name == "echo"));
    }
}
