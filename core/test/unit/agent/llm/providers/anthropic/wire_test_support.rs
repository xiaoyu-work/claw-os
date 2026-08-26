impl super::StreamConverter {
    pub(crate) fn debug_model(&self) -> &str {
        &self.model
    }

    pub(crate) fn debug_usage(&self) -> &crate::agent::llm::Usage {
        &self.usage
    }
}
