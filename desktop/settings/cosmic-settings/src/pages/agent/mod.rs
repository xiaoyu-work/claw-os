// SPDX-License-Identifier: GPL-3.0-only
//
// Top-level "Agent" settings page. Owns no state of its own; every modality
// is a sub-page that talks to `cos agent setup` over the kernel's
// non-interactive subcommands. See `common.rs` for the shared form.

pub mod common;
pub mod embed;
pub mod imagegen;
pub mod llm;
pub mod stt;
pub mod tts;

use cosmic_settings_page::{self as page};

pub use common::{Message, Modality};

#[derive(Default)]
pub struct Page {
    entity: page::Entity,
}

impl page::Page<crate::pages::Message> for Page {
    fn set_id(&mut self, entity: page::Entity) {
        self.entity = entity;
    }

    fn info(&self) -> page::Info {
        page::Info::new("agent", "applications-system-symbolic")
            .title(crate::fl!("agent"))
            .description(crate::fl!("agent", "desc"))
    }
}

impl page::AutoBind<crate::pages::Message> for Page {
    fn sub_pages(
        page: page::Insert<crate::pages::Message>,
    ) -> page::Insert<crate::pages::Message> {
        page.sub_page::<llm::Page>()
            .sub_page::<tts::Page>()
            .sub_page::<stt::Page>()
            .sub_page::<imagegen::Page>()
            .sub_page::<embed::Page>()
    }
}
