use super::*;
use crate::widget::segmented_button::{self, Appearance as SegAppearance};
use iced::Size;
use slotmap::SecondaryMap;
use std::collections::HashSet;

#[derive(Clone, Debug)]
enum TestMessage {}

struct TestVariant;

impl<SelectionMode, Message> SegmentedVariant
    for SegmentedButton<'_, TestVariant, SelectionMode, Message>
where
    Model<SelectionMode>: Selectable,
    SelectionMode: Default,
{
    const VERTICAL: bool = false;

    fn variant_appearance(
        _theme: &crate::Theme,
        _style: &crate::theme::SegmentedButton,
    ) -> SegAppearance {
        SegAppearance::default()
    }

    fn variant_bounds<'b>(
        &'b self,
        _state: &'b LocalState,
        bounds: Rectangle,
    ) -> Box<dyn Iterator<Item = ItemBounds> + 'b> {
        let len = self.model.order.len();
        if len == 0 {
            return Box::new(std::iter::empty());
        }
        let width = bounds.width / len as f32;
        Box::new(
            self.model
                .order
                .iter()
                .copied()
                .enumerate()
                .map(move |(idx, entity)| {
                    let rect = Rectangle {
                        x: bounds.x + (idx as f32) * width,
                        y: bounds.y,
                        width,
                        height: bounds.height,
                    };
                    ItemBounds::Button(entity, rect)
                }),
        )
    }

    fn variant_layout(
        &self,
        _state: &mut LocalState,
        _renderer: &crate::Renderer,
        _limits: &layout::Limits,
    ) -> Size {
        Size::ZERO
    }
}

fn sample_model() -> (
    segmented_button::SingleSelectModel,
    Vec<segmented_button::Entity>,
) {
    let mut entities = Vec::new();
    let model = segmented_button::Model::builder()
        .insert(|b| b.text("One").with_id(|id| entities.push(id)))
        .insert(|b| b.text("Two").with_id(|id| entities.push(id)))
        .insert(|b| b.text("Three").with_id(|id| entities.push(id)))
        .build();
    (model, entities)
}

fn test_state(dragging: segmented_button::Entity, len: usize) -> LocalState {
    let mut state = LocalState {
        menu_state: MenuBarState::default(),
        paragraphs: SecondaryMap::new(),
        text_hashes: SecondaryMap::new(),
        buttons_visible: 0,
        buttons_offset: 0,
        collapsed: false,
        focused: None,
        focused_item: Item::default(),
        focused_visible: false,
        hovered: Item::default(),
        known_length: 0,
        middle_clicked: None,
        internal_layout: Vec::new(),
        context_cursor: Point::ORIGIN,
        show_context: None,
        wheel_timestamp: None,
        dnd_state: crate::widget::dnd_destination::State::<Option<Entity>>::new(),
        fingers_pressed: HashSet::new(),
        pressed_item: None,
        tab_drag_candidate: None,
        dragging_tab: Some(dragging),
        drop_hint: None,
        offer_mimes: Vec::new(),
    };
    state.buttons_visible = len;
    state.known_length = len;
    state
}

#[test]
fn drop_hint_reports_before_and_after() {
    let (model, ids) = sample_model();
    let button =
        SegmentedButton::<TestVariant, segmented_button::SingleSelect, TestMessage>::new(
            &model,
        );
    let state = test_state(ids[0], model.order.len());
    let bounds = Rectangle {
        x: 0.0,
        y: 0.0,
        width: 300.0,
        height: 30.0,
    };
    let before = button
        .drop_hint_for_position(&state, bounds, Point::new(10.0, 15.0))
        .expect("hint");
    assert_eq!(before.entity, ids[0]);
    assert!(matches!(before.side, DropSide::Before));

    let after = button
        .drop_hint_for_position(&state, bounds, Point::new(290.0, 15.0))
        .expect("hint");
    assert_eq!(after.entity, ids[2]);
    assert!(matches!(after.side, DropSide::After));
}
