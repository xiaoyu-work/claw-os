// SPDX-License-Identifier: MIT
#include <linux/input-event-codes.h>
#include <math.h>
#include <stdlib.h>
#include <wayland-client.h>
#include <wayland-cursor.h>

#include "background-image.h"
#include "cairo.h"
#include "log.h"
#include "swaylock.h"

enum {
	FORM_WIDTH = 400, FORM_HEIGHT = 304, MARGIN = 26,
	FIELD_WIDTH = FORM_WIDTH - 2 * MARGIN, FIELD_Y = 150, FIELD_HEIGHT = 46,
	BUTTON_Y = 240, BUTTON_HEIGHT = 44,
};

struct layout {
	int x, y, width, height;
	double zoom;
};

static struct layout layout(const struct swaylock_surface *surface) {
	double zoom = fmin(1, fmin(surface->width / (FORM_WIDTH + 24.0),
			surface->height / (FORM_HEIGHT + 24.0)));
	int width = ceil(FORM_WIDTH * zoom);
	int height = ceil(FORM_HEIGHT * zoom);
	return (struct layout) {
		.x = (surface->width - width) / 2,
		.y = (surface->height - height) / 2,
		.width = width, .height = height, .zoom = zoom,
	};
}

static bool can_submit(const struct swaylock_state *state) {
	return state->auth_state != AUTH_STATE_VALIDATING && state->password.len > 0;
}

static void rounded(cairo_t *cr, double x, double y, double w, double h, double r) {
	const double quarter = acos(0);
	cairo_new_sub_path(cr);
	cairo_arc(cr, x + w - r, y + r, r, -quarter, 0);
	cairo_arc(cr, x + w - r, y + h - r, r, 0, quarter);
	cairo_arc(cr, x + r, y + h - r, r, quarter, 2 * quarter);
	cairo_arc(cr, x + r, y + r, r, 2 * quarter, 3 * quarter);
	cairo_close_path(cr);
}

static void text(cairo_t *cr, const char *label, double x, double y,
		int size, uint32_t color, bool bold, bool centered) {
	cairo_select_font_face(cr, "sans-serif", CAIRO_FONT_SLANT_NORMAL,
			bold ? CAIRO_FONT_WEIGHT_BOLD : CAIRO_FONT_WEIGHT_NORMAL);
	cairo_set_font_size(cr, size);
	cairo_set_source_u32(cr, color);
	if (centered) {
		cairo_text_extents_t extents;
		cairo_text_extents(cr, label, &extents);
		x -= extents.width / 2 + extents.x_bearing;
	}
	cairo_move_to(cr, x, y);
	cairo_show_text(cr, label);
}

static void paint_form(cairo_t *cr, enum auth_state auth, size_t characters, bool caps_lock) {
	bool checking = auth == AUTH_STATE_VALIDATING;
	bool rejected = auth == AUTH_STATE_INVALID && !characters;
	uint32_t border = rejected ? 0xffb6baff : 0xb3d1ffff;

	rounded(cr, 1, 1, FORM_WIDTH - 2, FORM_HEIGHT - 2, 22);
	cairo_set_source_u32(cr, 0x20283bff);
	cairo_fill_preserve(cr);
	cairo_set_source_u32(cr, 0x54637dff);
	cairo_set_line_width(cr, 1);
	cairo_stroke(cr);

	text(cr, "Claw OS", MARGIN, 39, 15, 0xb3d1ffff, true, false);
	text(cr, "Screen locked", MARGIN, 75, 28, 0xf1f5ffff, true, false);
	text(cr, "Use your sign-in password to unlock.", MARGIN, 104, 15, 0xcdd8ecff, false, false);
	text(cr, "Password", MARGIN, 139, 14, 0xf1f5ffff, true, false);

	rounded(cr, MARGIN, FIELD_Y, FIELD_WIDTH, FIELD_HEIGHT, 10);
	cairo_set_source_u32(cr, 0x121929ff);
	cairo_fill_preserve(cr);
	cairo_set_source_u32(cr, checking ? 0x54637dff : border);
	cairo_set_line_width(cr, 2);
	cairo_stroke(cr);

	double x = MARGIN + 15;
	double mid = FIELD_Y + FIELD_HEIGHT / 2.0;
	if (!checking) {
		if (!characters) {
			text(cr, "Enter password", x + 7, mid + 5, 15, 0xaab8d0ff, false, false);
		}
		cairo_set_source_u32(cr, 0xf1f5ffff);
		for (size_t i = 0; i < characters && x < MARGIN + FIELD_WIDTH - 24; ++i) {
			cairo_new_sub_path(cr);
			cairo_arc(cr, x + 4, mid, 3.5, 0, 4 * acos(0));
			x += 11;
		}
		cairo_fill(cr);
		cairo_rectangle(cr, x, mid - 10, 1.5, 20);
		cairo_fill(cr);
	}

	const char *hint = checking ? "Checking password..."
		: rejected ? "Password not accepted. Try again."
		: caps_lock ? "Caps Lock is on."
		: "Press Enter to unlock";
	text(cr, hint, MARGIN, 220, 14, rejected ? border : 0xcdd8ecff, false, false);
	bool enabled = !checking && characters > 0;
	rounded(cr, MARGIN, BUTTON_Y, FIELD_WIDTH, BUTTON_HEIGHT, 12);
	cairo_set_source_u32(cr, enabled ? 0xb3d1ffff : 0x374561ff);
	cairo_fill(cr);
	text(cr, "Unlock", FORM_WIDTH / 2, BUTTON_Y + 28, 15,
			enabled ? 0x14253fff : 0xcdd8ecff, true, true);
}

static bool render_form(struct swaylock_surface *surface) {
	struct layout box = layout(surface);
	int scale = surface->scale;
	const struct swaylock_state *state = surface->state;
	struct pool_buffer *buffer = get_next_buffer(state->shm,
			surface->indicator_buffers, box.width * scale, box.height * scale);
	if (!buffer) {
		if (!surface->indicator_buffers[0].busy || !surface->indicator_buffers[1].busy) {
			swaylock_log(LOG_ERROR, "Failed to allocate lock form buffer");
			exit(EXIT_FAILURE);
		}
		return false;
	}

	cairo_t *cr = buffer->cairo;
	cairo_identity_matrix(cr);
	cairo_set_operator(cr, CAIRO_OPERATOR_CLEAR);
	cairo_paint(cr);
	cairo_set_operator(cr, CAIRO_OPERATOR_OVER);
	cairo_scale(cr, box.zoom * scale, box.zoom * scale);
	paint_form(cr, state->auth_state, state->password.characters, state->xkb.caps_lock);

	wl_subsurface_set_position(surface->subsurface, box.x, box.y);
	wl_surface_set_buffer_scale(surface->child, scale);
	wl_surface_attach(surface->child, buffer->buffer, 0, 0);
	wl_surface_damage_buffer(surface->child, 0, 0, INT32_MAX, INT32_MAX);
	wl_surface_commit(surface->child);
	return true;
}

static void frame_done(void *data, struct wl_callback *callback, uint32_t time) {
	struct swaylock_surface *surface = data;
	wl_callback_destroy(callback);
	surface->frame = NULL;
	render(surface);
}

static const struct wl_callback_listener frame_listener = {.done = frame_done};

void render(struct swaylock_surface *surface) {
	int width = surface->width * surface->scale;
	int height = surface->height * surface->scale;
	if (!width || !height || !surface->dirty || surface->frame) {
		return;
	}

	struct swaylock_state *state = surface->state;
	struct pool_buffer background;
	bool resized = width != surface->last_buffer_width || height != surface->last_buffer_height;
	if (resized) {
		if (!create_buffer(state->shm, &background, width, height, WL_SHM_FORMAT_ARGB8888)) {
			swaylock_log(LOG_ERROR, "Failed to allocate lock background");
			exit(EXIT_FAILURE);
		}
		cairo_t *cr = background.cairo;
		cairo_set_source_u32(cr, state->args.colors.background);
		cairo_paint(cr);
		if (surface->image && state->args.mode != BACKGROUND_MODE_SOLID_COLOR) {
			render_background_image(cr, surface->image, state->args.mode, width, height);
		}
		wl_surface_attach(surface->surface, background.buffer, 0, 0);
		wl_surface_damage_buffer(surface->surface, 0, 0, INT32_MAX, INT32_MAX);
		surface->last_buffer_width = width;
		surface->last_buffer_height = height;
	}
	wl_surface_set_buffer_scale(surface->surface, surface->scale);
	surface->dirty = !render_form(surface);
	surface->frame = wl_surface_frame(surface->surface);
	wl_callback_add_listener(surface->frame, &frame_listener, surface);
	wl_surface_commit(surface->surface);
	if (resized) {
		destroy_buffer(&background);
	}
}

void claw_lock_pointer_enter(void *data, struct wl_pointer *pointer,
		uint32_t serial, struct wl_surface *surface, wl_fixed_t x, wl_fixed_t y) {
	struct swaylock_seat *seat = data;
	seat->pointer_focus = surface;
	seat->unlock_pressed = false;
	claw_lock_pointer_motion(data, pointer, 0, x, y);
	if (!seat->cursor_theme) {
		seat->cursor_theme = wl_cursor_theme_load(NULL, 24, seat->state->shm);
	}
	struct wl_cursor *cursor = seat->cursor_theme
		? wl_cursor_theme_get_cursor(seat->cursor_theme, "left_ptr") : NULL;
	if (cursor && cursor->image_count) {
		struct wl_cursor_image *image = cursor->images[0];
		if (!seat->cursor_surface) {
			seat->cursor_surface = wl_compositor_create_surface(seat->state->compositor);
		}
		wl_surface_attach(seat->cursor_surface, wl_cursor_image_get_buffer(image), 0, 0);
		wl_surface_damage(seat->cursor_surface, 0, 0, image->width, image->height);
		wl_surface_commit(seat->cursor_surface);
		wl_pointer_set_cursor(pointer, serial, seat->cursor_surface, image->hotspot_x, image->hotspot_y);
	}
}

void claw_lock_pointer_leave(void *data, struct wl_pointer *pointer,
		uint32_t serial, struct wl_surface *surface) {
	struct swaylock_seat *seat = data;
	seat->pointer_focus = NULL;
	seat->unlock_pressed = false;
}

void claw_lock_pointer_motion(void *data, struct wl_pointer *pointer,
		uint32_t time, wl_fixed_t x, wl_fixed_t y) {
	struct swaylock_seat *seat = data;
	seat->pointer_x = wl_fixed_to_double(x);
	seat->pointer_y = wl_fixed_to_double(y);
}

static bool pointer_on_unlock(const struct swaylock_seat *seat) {
	struct swaylock_surface *surface;
	wl_list_for_each(surface, &seat->state->surfaces, link) {
		if (!seat->pointer_focus || surface->surface != seat->pointer_focus
				|| !surface->width || !surface->height) {
			continue;
		}
		struct layout box = layout(surface);
		double x = (seat->pointer_x - box.x) / box.zoom;
		double y = (seat->pointer_y - box.y) / box.zoom;
		return x >= MARGIN && x < MARGIN + FIELD_WIDTH
			&& y >= BUTTON_Y && y < BUTTON_Y + BUTTON_HEIGHT;
	}
	return false;
}

void claw_lock_pointer_button(void *data, struct wl_pointer *pointer,
		uint32_t serial, uint32_t time, uint32_t button, uint32_t state) {
	struct swaylock_seat *seat = data;
	if (button != BTN_LEFT) {
		return;
	}
	bool inside = seat->state->locked && can_submit(seat->state) && pointer_on_unlock(seat);
	if (state == WL_POINTER_BUTTON_STATE_PRESSED) {
		seat->unlock_pressed = inside;
	} else {
		bool submit = seat->unlock_pressed && inside;
		seat->unlock_pressed = false;
		if (submit) {
			swaylock_handle_key(seat->state, XKB_KEY_Return, 0);
		}
	}
}
