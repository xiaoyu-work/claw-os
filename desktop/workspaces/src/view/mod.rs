use cosmic::Apply;
use cosmic::cosmic_theme::palette::WithAlpha;
use cosmic::cctk::cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_handle_v1;
use cosmic::cctk::cosmic_protocols::workspace::v2::client::zcosmic_workspace_handle_v2;
use cosmic::cctk::wayland_client::protocol::wl_output;
use cosmic::cctk::wayland_protocols::ext::workspace::v1::client::ext_workspace_handle_v1;
use cosmic::iced::advanced::layout::flex::Axis;
use cosmic::iced::clipboard::mime::{AllowedMimeTypes, AsMimeTypes};
use cosmic::iced::core::Shadow;
use cosmic::iced::core::text::{Ellipsize, EllipsizeHeightLimit};
use cosmic::iced::platform_specific::shell::subsurface_widget::Subsurface;
use cosmic::iced::widget::{column, row};
use cosmic::iced::{self, Alignment, Border, Length};
use cosmic::widget::{self, Widget};
use cosmic_comp_config::workspace::WorkspaceLayout;
use std::collections::HashSet;

use crate::backend::{self, CaptureImage};
use crate::dnd::{Drag, DragSurface, DragToplevel, DragWorkspace, DropTarget};
use crate::{App, LayerSurface, Msg, Toplevel, Workspace};

fn dnd_source_with_drag_surface<D: AsMimeTypes + Send + Clone + 'static>(
    drag_content: D,
    drag_surface: DragSurface,
    id: Option<iced::id::Id>,
    child: cosmic::Element<'_, Msg>,
    drag_icon: impl Fn() -> cosmic::Element<'static, Msg> + 'static,
) -> cosmic::Element<'_, Msg> {
    let mut source = cosmic::widget::dnd_source(child)
        .drag_threshold(5.)
        .drag_content(move || drag_content.clone())
        .drag_icon(move |offset| {
            (
                drag_icon().map(|_| ()),
                cosmic::iced::core::widget::tree::State::None,
                -offset,
            )
        })
        .on_start(Some(Msg::StartDrag(drag_surface)))
        .on_finish(Some(Msg::SourceFinished))
        .on_cancel(Some(Msg::SourceFinished));
    if let Some(id) = id {
        source.set_id(id);
    }
    source.into()
}

fn dnd_destination_for_target<T>(
    target: DropTarget,
    child: cosmic::Element<'_, Msg>,
    on_finish: impl Fn(T) -> Msg + 'static,
) -> cosmic::Element<'_, Msg>
where
    T: AllowedMimeTypes,
{
    let target2 = target.clone();
    cosmic::widget::dnd_destination::dnd_destination_for_data(
        child,
        move |data: Option<T>, _action| match data {
            Some(data) => on_finish(data),
            None => Msg::Ignore,
        },
    )
    .drag_id(target.drag_id())
    .on_enter(move |actions, mime, pos| Msg::DndEnter(target.clone(), actions, mime, pos))
    .on_leave(move || Msg::DndLeave(target2.clone()))
    .into()
}

pub(crate) fn layer_surface<'a>(
    app: &'a App,
    surface: &'a LayerSurface,
) -> cosmic::Element<'a, Msg> {
    let mut drag_toplevel = None;
    let mut drag_workspace = None;
    match &app.drag_surface {
        Some((DragSurface::Toplevel(handle), _)) => {
            drag_toplevel = Some(handle);
        }
        Some((DragSurface::Workspace(handle), _)) => {
            drag_workspace = Some(handle);
        }
        _ => {}
    }
    #[allow(clippy::mutable_key_type)]
    let workspaces_with_toplevels = app
        .toplevels
        .0
        .iter()
        .flat_map(|t| &t.info.workspace)
        .collect::<HashSet<_>>();
    let layout = app.conf.workspace_config.workspace_layout;
    let sidebar = workspaces_sidebar(
        app.workspaces.for_output(&surface.output),
        &workspaces_with_toplevels,
        &surface.output,
        layout,
        app.drop_target.as_ref(),
        drag_workspace,
    );
    let toplevels = toplevel_previews(
        app.toplevels.0.iter().filter(|i| {
            if !i.info.output.contains(&surface.output) {
                return false;
            }

            i.info.workspace.iter().any(|workspace| {
                app.workspaces
                    .for_handle(workspace)
                    .is_some_and(|x| x.is_active())
            })
        }),
        layout,
        drag_toplevel,
    );
    // TODO multiple active workspaces? Not currently supported by cosmic.
    let first_active_workspace = app
        .workspaces
        .for_output(&surface.output)
        .find(|w| w.is_active());
    let toplevels = if let Some(workspace) = first_active_workspace {
        dnd_destination_for_target(
            DropTarget::OutputToplevels(workspace.handle().clone(), surface.output.clone()),
            toplevels,
            Msg::DndToplevelDrop,
        )
    } else {
        // Shouldn't happen, but no drag destination if no active workspace found for output
        cosmic::Element::from(toplevels)
    };
    let container = match layout {
        WorkspaceLayout::Vertical => widget::layer_container(
            row![sidebar, toplevels]
                .spacing(12)
                .height(Length::Fill)
                .width(Length::Fill),
        ),
        WorkspaceLayout::Horizontal => widget::layer_container(
            column![sidebar, toplevels]
                .spacing(12)
                .height(Length::Fill)
                .width(Length::Fill),
        ),
    };

    let panel_regions = app.panel_regions(&surface.output);
    let container = widget::container(container).padding(panel_regions);

    let output = surface.output.clone();
    widget::mouse_area(container)
        .on_scroll(move |delta| Msg::OnScroll(output.clone(), delta))
        .into()
}

fn close_button(on_press: Msg) -> cosmic::Element<'static, Msg> {
    widget::button::custom(widget::icon::from_name("window-close-symbolic").size(16))
        .class(cosmic::theme::Button::Destructive)
        .on_press(on_press)
        .into()
}

fn pin_button_style(theme: &cosmic::Theme, is_pinned: bool) -> cosmic::widget::button::Style {
    let bg_color = if is_pinned {
        theme.cosmic().accent.base.into()
    } else {
        theme.cosmic().primary.base.into()
    };
    let icon_color = if is_pinned {
        theme.cosmic().accent.on.into()
    } else {
        theme.cosmic().primary.on.into()
    };
    cosmic::widget::button::Style {
        icon_color: Some(icon_color),
        background: Some(iced::Background::Color(bg_color)),
        border_radius: theme.cosmic().corner_radii.radius_m.into(),
        ..cosmic::widget::button::Style::new()
    }
}

fn pin_button(workspace: &Workspace) -> cosmic::Element<'static, Msg> {
    let is_pinned = workspace.is_pinned();
    crate::widgets::visibility_wrapper(
        widget::button::custom(
            widget::icon::from_name("pin-symbolic")
                .symbolic(true)
                .size(16),
        )
        .padding([4, 8])
        .class(cosmic::theme::Button::Custom {
            // TODO adjust state for hover, etc.
            active: Box::new(move |_, theme| pin_button_style(theme, is_pinned)),
            disabled: Box::new(move |theme| pin_button_style(theme, is_pinned)),
            hovered: Box::new(move |_, theme| pin_button_style(theme, is_pinned)),
            pressed: Box::new(move |_, theme| pin_button_style(theme, is_pinned)),
        })
        // TODO style selected correctly
        .selected(workspace.is_pinned())
        .on_press(Msg::TogglePinned(workspace.handle().clone())),
        // Show pin button only if hovered or pinned; but allocate space the same way
        // regardless
        (workspace.has_cursor || workspace.is_pinned())
            && workspace
                .info
                .cosmic_capabilities
                .contains(zcosmic_workspace_handle_v2::WorkspaceCapabilities::Pin),
    )
    .into()
}

fn workspace_close_button(workspace: &Workspace, is_empty: bool) -> cosmic::Element<'static, Msg> {
    crate::widgets::visibility_wrapper(
        widget::button::custom(
            widget::icon::from_name("window-close-symbolic")
                .symbolic(true)
                .size(16),
        )
        .padding([4, 8])
        .class(cosmic::theme::Button::Custom {
            active: Box::new(move |_, theme| pin_button_style(theme, false)),
            disabled: Box::new(move |theme| pin_button_style(theme, false)),
            hovered: Box::new(move |_, theme| pin_button_style(theme, false)),
            pressed: Box::new(move |_, theme| pin_button_style(theme, false)),
        })
        .on_press(Msg::CloseWorkspace(workspace.handle().clone())),
        // Reveal on hover, the way macOS shows the close badge on a Space —
        // and only for empty ones, because the compositor refuses to remove a
        // workspace that still has windows rather than take them down with it.
        workspace.has_cursor && is_empty,
    )
    .into()
}

/// Neutral chip for the trailing "add desktop" button. Deliberately not the
/// accent: the accent marks the *active* workspace, and spending it on a
/// permanent control would leave nothing to distinguish the one you are on.
fn add_button_style(theme: &cosmic::Theme) -> cosmic::widget::button::Style {
    let cosmic = theme.cosmic();
    cosmic::widget::button::Style {
        icon_color: Some(cosmic.on_bg_color().into()),
        background: Some(iced::Background::Color(
            cosmic.on_bg_color().with_alpha(0.08).into(),
        )),
        border_radius: cosmic.corner_radii.radius_m.into(),
        ..cosmic::widget::button::Style::new()
    }
}

fn spacing_for_add() -> [u16; 2] {
    let spacing = cosmic::theme::active().cosmic().spacing;
    [spacing.space_s, spacing.space_s]
}

fn workspace_item_appearance(
    theme: &cosmic::Theme,
    is_active: bool,
    hovered: bool,
) -> cosmic::widget::button::Style {
    let cosmic = theme.cosmic();
    let mut appearance = cosmic::widget::button::Style::new();
    // Claw Glass: large frosted radius on the workspace thumbnail card.
    appearance.border_radius = cosmic.radius_l().into();
    // Frosted translucent fill behind the live capture.
    appearance.background = Some(iced::Background::Color(cosmic.bg_component_color().into()));
    if is_active {
        // Brand-blue active ring — never gray. A crisp 2px accent stroke plus a
        // faint accent wash makes the selected workspace the one loud element.
        appearance.border_width = 2.0;
        appearance.border_color = cosmic.accent_color().into();
        appearance.background = Some(iced::Background::Color(
            cosmic.accent_color().with_alpha(0.12).into(),
        ));
    } else {
        // Inactive cards take a neutral hairline so the accent ring above is
        // the only blue in the overview.
        appearance.border_width = 1.0;
        appearance.border_color = cosmic.on_bg_color().with_alpha(0.12).into();
    }
    if hovered {
        // Hover feedback is a brand-blue wash, never the gray `button.base`.
        appearance.background = Some(iced::Background::Color(
            cosmic
                .accent_color()
                .with_alpha(if is_active { 0.16 } else { 0.08 })
                .into(),
        ));
    }
    appearance
}

fn workspace_item(
    workspace: &Workspace,
    _output: &wl_output::WlOutput,
    layout: WorkspaceLayout,
    is_drop_target: bool,
    has_workspace_drag: bool,
    has_toplevels: bool,
) -> cosmic::Element<'static, Msg> {
    let (mut image, image_height, image_width) = if let Some(img) = workspace.img.as_ref() {
        let is_rotated = matches!(
            img.transform,
            wl_output::Transform::_90
                | wl_output::Transform::_270
                | wl_output::Transform::Flipped90
                | wl_output::Transform::Flipped270
        );
        let (effective_width, effective_height) = if is_rotated {
            // If rotated, swap width and height
            (img.height, img.width)
        } else {
            (img.width, img.height)
        };

        if effective_width > effective_height {
            (
                // Landscape: fix height
                widget::container(capture_image(Some(img), 1.0)).max_height(126.0),
                126.0,
                126.0 * effective_width as f32 / effective_height as f32,
            )
        } else {
            (
                // Portrait: fix width
                widget::container(capture_image(Some(img), 1.0)).max_width(160),
                160.0 * effective_height as f32 / effective_width as f32,
                160.0,
            )
        }
    } else {
        (
            widget::container(capture_image(None, 1.0))
                .max_height(126.0)
                .max_width(224.0),
            126.0,
            224.0,
        )
    };

    let is_active = workspace.is_active() && !has_workspace_drag;

    let workspace_label = widget::text::body(fl!("workspace", number = workspace.info.name.as_str()))
        .ellipsize(Ellipsize::Middle(EllipsizeHeightLimit::Lines(1)));
    // Active workspace name reads in brand blue to reinforce selection;
    // inactive names use the default on-background text.
    let workspace_label = if is_active {
        workspace_label.class(cosmic::theme::Text::Accent)
    } else {
        workspace_label
    };
    let workspace_footer = row![
        // Close on the left, pin on the right: the label stays centred, and
        // both affordances only appear on hover.
        workspace_close_button(workspace, !has_toplevels),
        workspace_label.apply(widget::container).center_x(Length::Fill),
        pin_button(workspace),
    ];

    // Needed to prevent footer content getting pushed out when scaling on Vertical layout
    if layout == WorkspaceLayout::Vertical {
        image = image.height(Length::Fill);
    }
    let content = column![image, workspace_footer]
        .spacing(4)
        .align_x(Alignment::Center)
        .apply(widget::container)
        .max_height(image_height + 28.0)
        .max_width(image_width);

    // TODO editable name?
    let mut button = widget::button::custom(content)
        .selected(is_active)
        .class(cosmic::theme::Button::Custom {
            active: Box::new(move |_focused, theme| {
                workspace_item_appearance(theme, is_active, is_drop_target)
            }),
            disabled: Box::new(move |theme| {
                workspace_item_appearance(theme, is_active, is_drop_target)
            }),
            hovered: Box::new(move |_focused, theme| {
                workspace_item_appearance(theme, is_active, true)
            }),
            pressed: Box::new(move |_focused, theme| {
                workspace_item_appearance(theme, is_active, true)
            }),
        })
        .padding(8);
    if workspace
        .info
        .capabilities
        .contains(ext_workspace_handle_v1::WorkspaceCapabilities::Activate)
    {
        button = button.on_press(Msg::ActivateWorkspace(workspace.handle().clone()));
    }
    button.into()
}

fn workspace_drag_placeholder(
    other_workspace: &Workspace,
    other_output: &wl_output::WlOutput,
    layout: WorkspaceLayout,
) -> cosmic::Element<'static, Msg> {
    let drop_target = DropTarget::WorkspaceSidebarDragPlaceholder(
        other_workspace.handle().clone(),
        other_output.clone(),
    );
    let placeholder = widget::button::custom(
        widget::Space::new()
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .class(cosmic::theme::Button::Custom {
        active: Box::new(|_, _| unreachable!()),
        disabled: Box::new(|theme| workspace_item_appearance(theme, true, true)),
        hovered: Box::new(|_, _| unreachable!()),
        pressed: Box::new(|_, _| unreachable!()),
    })
    .padding(8);
    let placeholder = crate::widgets::match_size(
        workspace_item(other_workspace, other_output, layout, true, true, false),
        placeholder,
    );
    dnd_destination_for_target(drop_target, placeholder.into(), Msg::DndWorkspaceDrop)
}

fn workspace_sidebar_entry<'a>(
    workspace: &'a Workspace,
    output: &'a wl_output::WlOutput,
    layout: WorkspaceLayout,
    is_drop_target: bool,
    has_toplevels: bool,
    has_workspace_drag: bool,
) -> cosmic::Element<'a, Msg> {
    /* XXX
    let mouse_interaction = if is_drop_target {
        iced::mouse::Interaction::Crosshair
    } else {
        iced::mouse::Interaction::Idle
    };
    */
    let item = workspace_item(
        workspace,
        output,
        layout,
        is_drop_target,
        has_workspace_drag,
        has_toplevels,
    );
    let item = iced::widget::mouse_area(item)
        .on_enter(Msg::EnteredWorkspaceSidebarEntry(
            workspace.handle().clone(),
            true,
        ))
        .on_exit(Msg::EnteredWorkspaceSidebarEntry(
            workspace.handle().clone(),
            false,
        ));
    let workspace_clone = workspace.clone(); // TODO avoid clone
    let output_clone = output.clone();
    let drop_target = DropTarget::WorkspaceSidebarEntry(workspace.handle().clone(), output.clone());
    let destination =
        dnd_destination_for_target(drop_target, item.into(), |drag: Drag| match drag {
            Drag::Toplevel => Msg::DndToplevelDrop(DragToplevel {}),
            Drag::Workspace => Msg::DndWorkspaceDrop(DragWorkspace {}),
        });
    // Cosmic-comp auto-removes workspaces that aren't pinned and don't have toplevels when they
    // aren't the last workspace. So it shouldn't be possible to drag.
    if (has_toplevels || workspace.is_pinned())
        && workspace
            .info
            .cosmic_capabilities
            .contains(zcosmic_workspace_handle_v2::WorkspaceCapabilities::Move)
    {
        dnd_source_with_drag_surface(
            DragWorkspace {},
            DragSurface::Workspace(workspace.handle().clone()),
            Some(workspace.dnd_source_id.clone()),
            destination,
            move || workspace_item(&workspace_clone, &output_clone, layout, false, true, false),
        )
    } else {
        destination
    }
}

#[allow(clippy::mutable_key_type)]
fn workspaces_sidebar<'a>(
    workspaces: impl Iterator<Item = &'a Workspace>,
    workspaces_with_toplevels: &HashSet<&backend::ExtWorkspaceHandleV1>,
    output: &'a wl_output::WlOutput,
    layout: WorkspaceLayout,
    drop_target: Option<&DropTarget>,
    drag_workspace: Option<&'a backend::ExtWorkspaceHandleV1>,
) -> cosmic::Element<'a, Msg> {
    let mut sidebar_entries = Vec::new();
    let mut last_handle = None;
    for workspace in workspaces {
        // XXX Need dnd source with same id for drag to work; but give it 0x0 size
        if drag_workspace == Some(workspace.handle()) {
            let workspace_clone = workspace.clone();
            let output_clone = output.clone();
            let source = dnd_source_with_drag_surface(
                DragWorkspace {},
                DragSurface::Workspace(workspace.handle().clone()),
                Some(workspace.dnd_source_id.clone()),
                widget::Space::new()
                    .width(Length::Shrink)
                    .height(Length::Shrink)
                    .into(),
                move || workspace_item(&workspace_clone, &output_clone, layout, false, true, false),
            );
            sidebar_entries.push(source);
            continue;
        }

        let mut drop_target_is_workspace = false;
        let mut drop_target_is_placeholder = false;
        match drop_target {
            Some(DropTarget::WorkspaceSidebarEntry(w, o))
                if (w, o) == (workspace.handle(), output) =>
            {
                drop_target_is_workspace = true;
            }
            Some(DropTarget::WorkspaceSidebarDragPlaceholder(w, o))
                if (w, o) == (workspace.handle(), output) =>
            {
                drop_target_is_placeholder = true;
            }
            _ => {}
        }

        if drag_workspace.is_some()
            && drag_workspace != Some(workspace.handle())
            && (drop_target_is_workspace || drop_target_is_placeholder)
        {
            sidebar_entries.push(workspace_drag_placeholder(workspace, output, layout));
        }
        sidebar_entries.push(workspace_sidebar_entry(
            workspace,
            output,
            layout,
            drop_target_is_workspace && drag_workspace.is_none(),
            workspaces_with_toplevels.contains(workspace.handle()),
            drag_workspace.is_some(),
        ));
        last_handle = Some(workspace.handle().clone());
    }
    // Trailing "+", the way macOS puts Add Desktop at the end of the Spaces
    // bar. The protocol request is made against a workspace group, and the
    // group is reached through a workspace it owns, so this needs one existing
    // workspace to hang off — there is always at least one.
    if drag_workspace.is_none()
        && let Some(handle) = last_handle
    {
        sidebar_entries.push(
            widget::button::custom(
                widget::icon::from_name("list-add-symbolic")
                    .symbolic(true)
                    .size(16),
            )
            .padding(spacing_for_add())
            .class(cosmic::theme::Button::Custom {
                active: Box::new(|_, theme| add_button_style(theme)),
                disabled: Box::new(|theme| add_button_style(theme)),
                hovered: Box::new(|_, theme| add_button_style(theme)),
                pressed: Box::new(|_, theme| add_button_style(theme)),
            })
            .on_press(Msg::NewWorkspace(handle))
            .into(),
        );
    }
    let (axis, width, height) = match layout {
        WorkspaceLayout::Vertical => (Axis::Vertical, Length::Shrink, Length::Fill),
        WorkspaceLayout::Horizontal => (Axis::Horizontal, Length::Fill, Length::Shrink),
    };
    let spacing = cosmic::theme::active().cosmic().spacing;
    let sidebar_entries_container =
        widget::container(crate::widgets::workspace_bar(sidebar_entries, axis))
            .padding(spacing.space_xs);

    widget::container(
        widget::container(sidebar_entries_container)
            .width(width)
            .height(height)
            .class(cosmic::theme::Container::custom(|theme| {
                let cosmic = theme.cosmic();
                // Claw Glass: frosted translucent bar floating over the
                // blurred desktop — never an opaque gray field. The fill has
                // to actually carry alpha to be frosted; at full opacity this
                // read as a flat black slab pinned beside the dock.
                let mut fill = cosmic.bg_component_color();
                fill.alpha = 0.74;
                cosmic::iced::widget::container::Style {
                    text_color: Some(cosmic.on_bg_color().into()),
                    icon_color: Some(cosmic.on_bg_color().into()),
                    background: Some(iced::Background::Color(fill.into())),
                    border: Border {
                        // Large frosted radius for the floating overview bar.
                        radius: cosmic.radius_l().into(),
                        // 1px neutral hairline; depth comes from the blur.
                        width: 1.0,
                        color: cosmic.on_bg_color().with_alpha(0.10).into(),
                    },
                    shadow: Shadow::default(),
                    snap: true,
                }
            })),
    )
    .padding(spacing.space_xs)
    .into()
}

fn toplevel_preview(toplevel: &Toplevel, is_being_dragged: bool) -> cosmic::Element<'static, Msg> {
    let cosmic::cosmic_theme::Spacing {
        space_xxs, space_s, ..
    } = cosmic::theme::active().cosmic().spacing;

    let label = widget::text::body(toplevel.info.title.clone())
        .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)));
    let label = if let Some(icon) = &toplevel.icon {
        row![
            widget::icon(widget::icon::from_path(icon.clone())).size(24),
            label
        ]
        .spacing(4)
    } else {
        row![label]
    }
    .align_y(Alignment::Center);

    let title = row![
        widget::button::custom(label)
            .on_press(Msg::ActivateToplevel(toplevel.handle.clone()))
            .class(cosmic::theme::Button::Icon)
            .padding([space_xxs, space_s])
            .apply(widget::container)
            .class(cosmic::theme::Container::custom(|theme| {
                cosmic::iced::widget::container::Style {
                    background: Some(
                        iced::Color::from(theme.cosmic().background.component.base).into(),
                    ),
                    border: Border {
                        color: theme.cosmic().bg_divider().into(),
                        width: 1.0,
                        radius: theme.cosmic().radius_xl().into(),
                    },
                    ..Default::default()
                }
            }))
            .apply(widget::container)
            .width(Length::Fill),
        close_button(Msg::CloseToplevel(toplevel.handle.clone()))
    ]
    .spacing(8)
    .padding([0, 0, 2, 0])
    .align_y(Alignment::Center);

    let alpha = if is_being_dragged { 0.5 } else { 1.0 };
    let preview = widget::button::custom(capture_image(toplevel.img.as_ref(), alpha))
        .selected(
            toplevel
                .info
                .state
                .contains(&zcosmic_toplevel_handle_v1::State::Activated),
        )
        .class(cosmic::theme::Button::Image)
        .on_press(Msg::ActivateToplevel(toplevel.handle.clone()));

    widget::mouse_area(crate::widgets::size_cross_nth(
        vec![title.into(), preview.into()],
        Axis::Vertical,
        1, // Allocate width to match capture image
    ))
    .on_middle_press(Msg::CloseToplevel(toplevel.handle.clone()))
    .into()
}

fn toplevel_previews_entry(
    toplevel: &Toplevel,
    is_being_dragged: bool,
) -> cosmic::Element<'_, Msg> {
    // Dragged window still takes up space until moved, but isn't rendered while drag surface is
    // shown.
    let preview = crate::widgets::visibility_wrapper(
        toplevel_preview(toplevel, is_being_dragged),
        !is_being_dragged,
    );
    let toplevel2 = toplevel.clone();
    dnd_source_with_drag_surface(
        DragToplevel {},
        DragSurface::Toplevel(toplevel.handle.clone()),
        None,
        preview.into(),
        move || toplevel_preview(&toplevel2, true),
    )
}

fn toplevel_previews<'a>(
    toplevels: impl Iterator<Item = &'a Toplevel>,
    layout: WorkspaceLayout,
    drag_toplevel: Option<&'a backend::ExtForeignToplevelHandleV1>,
) -> cosmic::Element<'a, Msg> {
    let (width, height) = match layout {
        WorkspaceLayout::Vertical => (Length::FillPortion(4), Length::Fill),
        WorkspaceLayout::Horizontal => (Length::Fill, Length::FillPortion(4)),
    };
    let entries = toplevels
        .map(|t| toplevel_previews_entry(t, drag_toplevel == Some(&t.handle)))
        .collect();
    //row(entries)
    widget::mouse_area(
        widget::container(crate::widgets::toplevels(entries))
            .align_x(Alignment::Center)
            .width(width)
            .height(height)
            .padding(12),
    )
    .on_press(Msg::Close)
    .into()
}

fn capture_image(image: Option<&CaptureImage>, alpha: f32) -> cosmic::Element<'static, Msg> {
    if let Some(image) = image {
        #[cfg(feature = "no-subsurfaces")]
        {
            // TODO alpha, transform
            widget::Image::new(image.image.clone()).into()
        }
        #[cfg(not(feature = "no-subsurfaces"))]
        {
            Subsurface::new(image.wl_buffer.clone())
                .alpha(alpha)
                .transform(image.transform)
                .into()
        }
    } else {
        widget::Image::new(widget::image::Handle::from_rgba(1, 1, vec![0, 0, 0, 255])).into()
    }
}
