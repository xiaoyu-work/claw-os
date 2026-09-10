// SPDX-License-Identifier: MPL-2.0

use std::cell::RefCell;

use iced::widget::{Image, image};
use iced_core::{
    ContentFit, Layout, Length, Rectangle, Rotation, Size, Widget,
    image::Handle,
    layout, mouse, renderer,
    widget::{Tree, tree},
};

pub(super) struct Symbolic {
    image: Image<'static>,
    source: Handle,
    content_fit: ContentFit,
    rotation: Rotation,
}

#[derive(Default)]
struct Cache(Option<(Handle, [u8; 4], Handle)>);

impl Cache {
    fn handle(&mut self, source: &Handle, color: [u8; 4]) -> Handle {
        if let Some((previous, previous_color, handle)) = &self.0 {
            if previous == source && *previous_color == color {
                return handle.clone();
            }
        }
        let Handle::Rgba {
            width,
            height,
            pixels,
            ..
        } = source
        else {
            unreachable!("symbolic raster widgets accept decoded RGBA only")
        };
        let mut pixels = pixels.to_vec();
        for pixel in pixels.chunks_exact_mut(4) {
            if pixel[3] > 0 {
                pixel[..3].copy_from_slice(&color[..3]);
            }
        }
        let handle = Handle::from_rgba(*width, *height, pixels);
        self.0 = Some((source.clone(), color, handle.clone()));
        handle
    }
}

impl Symbolic {
    pub(super) fn new(
        image: Image<'static>,
        source: Handle,
        content_fit: ContentFit,
        rotation: Rotation,
    ) -> Self {
        Self {
            image,
            source,
            content_fit,
            rotation,
        }
    }
}

impl<Message> Widget<Message, crate::Theme, crate::Renderer> for Symbolic {
    fn size(&self) -> Size<Length> {
        <Image<'_> as Widget<Message, crate::Theme, crate::Renderer>>::size(&self.image)
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<RefCell<Cache>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(RefCell::new(Cache::default()))
    }

    fn id(&self) -> Option<iced_core::id::Id> {
        <Image<'_> as Widget<Message, crate::Theme, crate::Renderer>>::id(&self.image)
    }

    fn set_id(&mut self, id: iced_core::id::Id) {
        <Image<'_> as Widget<Message, crate::Theme, crate::Renderer>>::set_id(&mut self.image, id);
    }

    #[cfg(feature = "a11y")]
    fn a11y_nodes(
        &self,
        layout: Layout<'_>,
        tree: &Tree,
        cursor: mouse::Cursor,
    ) -> iced_accessibility::A11yTree {
        <Image<'_> as Widget<Message, crate::Theme, crate::Renderer>>::a11y_nodes(
            &self.image,
            layout,
            tree,
            cursor,
        )
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &crate::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        <Image<'_> as Widget<Message, crate::Theme, crate::Renderer>>::layout(
            &mut self.image,
            tree,
            renderer,
            limits,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut crate::Renderer,
        _theme: &crate::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let handle = tree
            .state
            .downcast_ref::<RefCell<Cache>>()
            .borrow_mut()
            .handle(&self.source, style.icon_color.into_rgba8());
        image::draw(
            renderer,
            layout,
            &handle,
            None,
            [0.0; 4].into(),
            self.content_fit,
            image::FilterMethod::default(),
            self.rotation,
            1.0,
            1.0,
        );
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/widget/icon/raster.rs"
    ));
}
