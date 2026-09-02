use crate::bridge::{BridgeEndpoint, ModelsResponse};

#[derive(Debug, Default)]
pub(crate) struct BridgeState {
    endpoint: Option<BridgeEndpoint>,
    error: Option<String>,
    connecting: bool,
    models: Option<ModelsResponse>,
}

impl BridgeState {
    pub(crate) fn connecting() -> Self {
        Self {
            connecting: true,
            ..Self::default()
        }
    }

    pub(crate) fn endpoint(&self) -> Option<&BridgeEndpoint> {
        self.endpoint.as_ref()
    }

    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub(crate) fn models(&self) -> Option<&ModelsResponse> {
        self.models.as_ref()
    }

    pub(crate) fn is_connecting(&self) -> bool {
        self.connecting
    }

    pub(crate) fn begin_connect(&mut self) -> bool {
        if self.connecting {
            return false;
        }
        self.connecting = true;
        true
    }

    pub(crate) fn connected(&mut self, endpoint: BridgeEndpoint) {
        self.connecting = false;
        self.error = None;
        self.endpoint = Some(endpoint);
    }

    pub(crate) fn connection_failed(&mut self, error: String) {
        self.connecting = false;
        self.endpoint = None;
        self.models = None;
        self.error = Some(error);
    }

    pub(crate) fn transport_failed(&mut self, error: String) {
        self.endpoint = None;
        self.models = None;
        self.error = Some(error);
        self.connecting = true;
    }

    pub(crate) fn models_loaded(&mut self, models: ModelsResponse) {
        self.models = Some(models);
    }

    pub(crate) fn models_failed(&mut self, error: String) {
        self.error = Some(error);
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/bridge_state.rs"
    ));
}
