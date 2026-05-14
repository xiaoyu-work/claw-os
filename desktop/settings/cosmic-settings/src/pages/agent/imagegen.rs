// SPDX-License-Identifier: GPL-3.0-only
//
// Image generation modality sub-page. All real logic lives in `super::common`.

use cosmic::Task;
use cosmic_settings_page::{self as page, Section, section};
use slotmap::SlotMap;

use super::common::{self, Modality, State};

#[derive(Clone, Debug)]
pub struct Page {
    entity: page::Entity,
    state: State,
}

impl Default for Page {
    fn default() -> Self {
        Self {
            entity: page::Entity::default(),
            state: State::new(Modality::Imagegen),
        }
    }
}

impl page::AutoBind<crate::pages::Message> for Page {}

impl page::Page<crate::pages::Message> for Page {
    fn set_id(&mut self, entity: page::Entity) {
        self.entity = entity;
    }

    fn info(&self) -> page::Info {
        page::Info::new(Modality::Imagegen.page_id(), Modality::Imagegen.icon_name())
            .title(Modality::Imagegen.title())
            .description(Modality::Imagegen.description())
    }

    fn content(
        &self,
        sections: &mut SlotMap<section::Entity, Section<crate::pages::Message>>,
    ) -> Option<page::Content> {
        Some(vec![
            sections.insert(common::status_section::<Page, _>(|p| &p.state, wrap)),
            sections.insert(common::provider_section::<Page, _>(|p| &p.state, wrap)),
            sections.insert(common::actions_section::<Page, _>(|p| &p.state, wrap)),
        ])
    }

    fn on_enter(&mut self) -> Task<crate::pages::Message> {
        self.state.on_enter(wrap)
    }
}

impl Page {
    pub fn update(&mut self, message: common::Message) -> Task<crate::Message> {
        self.state.update(message, wrap).map(Into::into)
    }
}

fn wrap(message: common::Message) -> crate::pages::Message {
    crate::pages::Message::Agent(Modality::Imagegen, message)
}
