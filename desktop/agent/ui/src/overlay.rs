use cosmic::app::Task;
use cosmic::iced::Limits;
use cosmic::iced::platform_specific::runtime::wayland::layer_surface::{
    IcedMargin, SctkLayerSurfaceSettings,
};
use cosmic::iced::platform_specific::shell::commands::layer_surface::{
    Anchor, KeyboardInteractivity, Layer, destroy_layer_surface, get_layer_surface,
};
use cosmic::iced::window::Id as SurfaceId;

use crate::Message;
pub(crate) use cos_runtime::ask_claw::Activation as OverlayActivation;

#[derive(Debug, Clone)]
pub(crate) struct DeferredSubmit {
    pub(crate) session_index: usize,
    pub(crate) prompt: String,
    pub(crate) context: Option<String>,
    pub(crate) activation_generation: u64,
}

#[derive(Debug)]
pub(crate) struct OverlayState {
    visible: bool,
    activation_generation: u64,
    pending_context: Option<String>,
    stream_context_generation: Option<u64>,
    auto_submit: bool,
    deferred_submit: Option<DeferredSubmit>,
    file_picker_open: bool,
}

impl OverlayState {
    pub(crate) fn new(visible: bool, context: Option<String>, auto_submit: bool) -> Self {
        Self {
            visible,
            activation_generation: 0,
            pending_context: context,
            stream_context_generation: None,
            auto_submit,
            deferred_submit: None,
            file_picker_open: false,
        }
    }

    pub(crate) fn is_visible(&self) -> bool {
        self.visible
    }

    pub(crate) fn activation_generation(&self) -> u64 {
        self.activation_generation
    }

    pub(crate) fn pending_context(&self) -> Option<&str> {
        self.pending_context.as_deref()
    }

    pub(crate) fn set_pending_context(&mut self, context: Option<String>) {
        self.pending_context = context;
    }

    pub(crate) fn auto_submit(&self) -> bool {
        self.auto_submit
    }

    pub(crate) fn take_auto_submit(&mut self) -> bool {
        std::mem::take(&mut self.auto_submit)
    }

    pub(crate) fn defer_submit(&mut self, session_index: usize, prompt: String) {
        self.deferred_submit = Some(DeferredSubmit {
            session_index,
            prompt,
            context: self.pending_context.clone(),
            activation_generation: self.activation_generation,
        });
    }

    pub(crate) fn take_deferred_submit(&mut self) -> Option<DeferredSubmit> {
        self.deferred_submit.take()
    }

    pub(crate) fn begin_stream_context(&mut self, has_context: bool) {
        self.stream_context_generation = has_context.then_some(self.activation_generation);
    }

    pub(crate) fn consume_stream_context(&mut self) {
        if self.stream_context_generation == Some(self.activation_generation) {
            self.pending_context = None;
        }
        self.stream_context_generation = None;
    }

    pub(crate) fn begin_activation(&mut self, activation: OverlayActivation) {
        self.activation_generation = self.activation_generation.wrapping_add(1);
        self.pending_context = activation.context;
        self.auto_submit = activation.query.is_some();
    }

    pub(crate) fn file_picker_open(&self) -> bool {
        self.file_picker_open
    }

    pub(crate) fn set_file_picker_open(&mut self, open: bool) {
        self.file_picker_open = open;
    }

    pub(crate) fn open(&mut self, id: SurfaceId) -> Task<Message> {
        self.visible = true;
        get_layer_surface(SctkLayerSurfaceSettings {
            id,
            layer: Layer::Overlay,
            keyboard_interactivity: KeyboardInteractivity::Exclusive,
            anchor: Anchor::TOP,
            output: Default::default(),
            namespace: "clawos-agent".into(),
            margin: IcedMargin {
                top: 72,
                ..Default::default()
            },
            size: None,
            exclusive_zone: 0,
            size_limits: Limits::NONE
                .min_width(1.0)
                .min_height(120.0)
                .max_width(560.0)
                .max_height(560.0),
            ..Default::default()
        })
    }

    pub(crate) fn close(&mut self, id: SurfaceId) -> Task<Message> {
        if !self.visible {
            return Task::none();
        }
        self.reset();
        destroy_layer_surface(id)
    }

    pub(crate) fn layer_done(&mut self) {
        self.reset();
    }

    fn reset(&mut self) {
        self.visible = false;
        self.activation_generation = self.activation_generation.wrapping_add(1);
        self.auto_submit = false;
        self.deferred_submit = None;
        self.pending_context = None;
        self.stream_context_generation = None;
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/overlay.rs"));
}
