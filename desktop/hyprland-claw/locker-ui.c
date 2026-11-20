// SPDX-License-Identifier: MIT
#include <errno.h>
#include <linux/input-event-codes.h>
#include <math.h>
#include <pango/pangocairo.h>
#include <poll.h>
#include <pwd.h>
#include <stdlib.h>
#include <string.h>
#include <sys/timerfd.h>
#include <time.h>
#include <unistd.h>
#include <wayland-client.h>
#include <wayland-cursor.h>

#include "background-image.h"
#include "cairo.h"
#include "log.h"
#include "loop.h"
#include "swaylock.h"

enum {
	CONTENT_WIDTH = 360,
	FIELD_WIDTH = 280, FIELD_HEIGHT = 44,
	FIELD_X = (CONTENT_WIDTH - FIELD_WIDTH) / 2,
	BUTTON_X = FIELD_X + FIELD_WIDTH - FIELD_HEIGHT,
};

static const char *account = "Screen locked";
static cairo_surface_t *wallpaper;
static cairo_surface_t *logo;

struct layout {
	int x, y, width, height;
	double zoom, clock_size, field_y;
};

static struct layout layout(const struct swaylock_surface *surface) {
	double zoom = fmin(1, fmin(surface->width / 384.0, surface->height / 440.0));
	double scene_height = surface->height / zoom;
	int width = ceil(CONTENT_WIDTH * zoom);
	int top = round(fmin(112, scene_height * 0.09) * zoom);
	double bottom = fmin(106, fmax(64, round(scene_height * 0.1)));
	return (struct layout) {
		.x = (surface->width - width) / 2,
		.y = top,
		.width = width,
		.height = surface->height - top - floor(zoom * 14),
		.zoom = zoom,
		.clock_size = round(fmin(108, scene_height * 0.135)),
		.field_y = (surface->height - top) / zoom - bottom - FIELD_HEIGHT,
	};
}

static bool arm_clock(int fd) {
	time_t now = time(NULL);
	struct itimerspec next = {.it_value.tv_sec = now - now % 60 + 60};
	return timerfd_settime(fd, TFD_TIMER_ABSTIME | TFD_TIMER_CANCEL_ON_SET, &next, NULL) == 0;
}

static void clock_tick(int fd, short events, void *data) {
	uint64_t ticks;
	if (read(fd, &ticks, sizeof(ticks)) < 0 && errno != ECANCELED) {
		return;
	}
	if (!arm_clock(fd)) {
		swaylock_log_errno(LOG_ERROR, "Failed to schedule lock clock");
	}
	damage_state(data);
}

void claw_lock_style_start(struct swaylock_state *state) {
	wallpaper = load_background_image("/usr/share/backgrounds/claw-os/claw-default.jpg");
	logo = cairo_image_surface_create_from_png("/usr/share/claw-os/oobe/logo.png");

	const struct passwd *owner = getpwuid(getuid());
	if (owner) {
		const char *name = owner->pw_gecos;
		size_t length = name ? strcspn(name, ",") : 0;
		account = length ? g_utf8_make_valid(name, length) : g_utf8_make_valid(owner->pw_name, -1);
	}

	// A wall-clock deadline also advances while the machine is suspended.
	int fd = timerfd_create(CLOCK_REALTIME, TFD_CLOEXEC | TFD_NONBLOCK);
	if (fd >= 0 && arm_clock(fd)) {
		loop_add_fd(state->eventloop, fd, POLLIN, clock_tick, state);
	} else {
		swaylock_log_errno(LOG_ERROR, "Failed to start lock clock");
		if (fd >= 0) {
			close(fd);
		}
	}
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

static void text(cairo_t *cr, const char *label, double x, double y, double width,
		int size, uint32_t color, bool emphasized, PangoAlignment alignment) {
	PangoLayout *line = pango_cairo_create_layout(cr);
	PangoFontDescription *font = pango_font_description_new();
	pango_font_description_set_family(font, "Inter");
	pango_font_description_set_weight(font, emphasized ? PANGO_WEIGHT_SEMIBOLD : PANGO_WEIGHT_NORMAL);
	pango_font_description_set_absolute_size(font, size * PANGO_SCALE);
	pango_layout_set_font_description(line, font);
	pango_font_description_free(font);
	pango_layout_set_text(line, label, -1);
	pango_layout_set_single_paragraph_mode(line, TRUE);
	pango_layout_set_width(line, width * PANGO_SCALE);
	pango_layout_set_ellipsize(line, PANGO_ELLIPSIZE_END);
	pango_layout_set_alignment(line, alignment);
	int height;
	pango_layout_get_pixel_size(line, NULL, &height);
	cairo_set_source_u32(cr, color);
	cairo_move_to(cr, x, y - height / 2.0);
	pango_cairo_show_layout(cr, line);
	g_object_unref(line);
}

static void paint_clock(cairo_t *cr, const struct layout *box) {
	time_t now = time(NULL);
	struct tm local;
	if (localtime_r(&now, &local)) {
		char date[80], time_label[16];
		strftime(date, sizeof(date), "%A, %B %-d", &local);
		strftime(time_label, sizeof(time_label), "%H:%M", &local);
		text(cr, date, 0, 12, CONTENT_WIDTH, 20, 0xf4f8ffff, true, PANGO_ALIGN_CENTER);
		text(cr, time_label, 0, 23 + box->clock_size / 2, CONTENT_WIDTH,
				box->clock_size, 0xf4f8ffff, true, PANGO_ALIGN_CENTER);
	}
}

static void paint_avatar(cairo_t *cr, double y) {
	double x = CONTENT_WIDTH / 2.0;
	cairo_new_sub_path(cr);
	cairo_arc(cr, x, y, 30.5, 0, 4 * acos(0));
	cairo_set_source_u32(cr, 0xe7edf4ff);
	cairo_fill_preserve(cr);
	cairo_set_source_u32(cr, 0xffffffb3);
	cairo_set_line_width(cr, 1);
	cairo_stroke(cr);
	if (cairo_surface_status(logo) == CAIRO_STATUS_SUCCESS) {
		cairo_save(cr);
		cairo_translate(cr, x - 18, y - 18);
		cairo_scale(cr, 36.0 / cairo_image_surface_get_width(logo),
				36.0 / cairo_image_surface_get_height(logo));
		cairo_set_source_surface(cr, logo, 0, 0);
		cairo_paint(cr);
		cairo_restore(cr);
	}
}

static void paint_form(cairo_t *cr, const struct layout *box, enum auth_state auth,
		size_t characters, bool caps_lock) {
	bool checking = auth == AUTH_STATE_VALIDATING;
	bool rejected = auth == AUTH_STATE_INVALID && !characters;
	bool enabled = !checking && characters > 0;
	double y = box->field_y, mid = y + FIELD_HEIGHT / 2.0;
	uint32_t border = rejected ? 0xffc4c0ff : 0xffffffb3;

	paint_avatar(cr, y - 79);
	text(cr, account, FIELD_X, y - 24, FIELD_WIDTH, 18, 0xf4f8ffff, true, PANGO_ALIGN_CENTER);

	rounded(cr, FIELD_X + 0.5, y + 0.5, FIELD_WIDTH - 1, FIELD_HEIGHT - 1, (FIELD_HEIGHT - 1) / 2.0);
	cairo_set_source_u32(cr, 0x151c26c4);
	cairo_fill_preserve(cr);
	cairo_set_source_u32(cr, checking ? 0xffffff55 : border);
	cairo_set_line_width(cr, 1);
	cairo_stroke(cr);

	double x = FIELD_X + 17;
	if (!characters) {
		text(cr, "Password", x + (checking ? 0 : 7), mid, BUTTON_X - x - 10,
				15, 0xd7e3efff, false, PANGO_ALIGN_LEFT);
	}
	if (!checking) {
		cairo_set_source_u32(cr, 0xf4f8ffff);
		for (size_t i = 0; i < characters && x < BUTTON_X - 13; ++i) {
			cairo_new_sub_path(cr);
			cairo_arc(cr, x + 4, mid, 3.2, 0, 4 * acos(0));
			x += 11;
		}
		cairo_fill(cr);
		cairo_rectangle(cr, x, mid - 9, 1, 18);
		cairo_fill(cr);
	}

	double arrow = BUTTON_X + FIELD_HEIGHT / 2.0;
	cairo_new_sub_path(cr);
	cairo_arc(cr, arrow, mid, 17, 0, 4 * acos(0));
	cairo_set_source_u32(cr, enabled ? 0xedf5ffff : 0xffffff24);
	cairo_fill(cr);
	cairo_move_to(cr, arrow - 6, mid);
	cairo_line_to(cr, arrow + 6, mid);
	cairo_move_to(cr, arrow + 1, mid - 5);
	cairo_line_to(cr, arrow + 6, mid);
	cairo_line_to(cr, arrow + 1, mid + 5);
	cairo_set_source_u32(cr, enabled ? 0x172431ff : 0xb8c5d3ff);
	cairo_set_line_width(cr, 1.6);
	cairo_set_line_cap(cr, CAIRO_LINE_CAP_ROUND);
	cairo_stroke(cr);

	const char *hint = checking ? "Checking password..."
		: rejected ? "Password not accepted. Try again."
		: caps_lock ? "Caps Lock is on." : NULL;
	if (hint) {
		text(cr, hint, FIELD_X, y + FIELD_HEIGHT + 16, FIELD_WIDTH, 14,
				rejected ? border : 0xd7e3efff, false, PANGO_ALIGN_CENTER);
	}
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
	paint_clock(cr, &box);
	paint_form(cr, &box, state->auth_state, state->password.characters, state->xkb.caps_lock);

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
		cairo_set_source_u32(cr, state->args.colors.background | 0xffu);
		cairo_paint(cr);
		if (surface->image && state->args.mode != BACKGROUND_MODE_SOLID_COLOR) {
			render_background_image(cr, surface->image, state->args.mode, width, height);
		} else if (wallpaper) {
			render_background_image(cr, wallpaper, BACKGROUND_MODE_FILL, width, height);
		}
		cairo_pattern_t *scrim = cairo_pattern_create_linear(0, 0, 0, height);
		cairo_pattern_add_color_stop_rgba(scrim, 0.0, 16/255., 25/255., 35/255., 171/255.);
		cairo_pattern_add_color_stop_rgba(scrim, 0.3, 16/255., 25/255., 35/255., 99/255.);
		cairo_pattern_add_color_stop_rgba(scrim, 0.59, 16/255., 25/255., 35/255., 22/255.);
		cairo_pattern_add_color_stop_rgba(scrim, 1.0, 16/255., 25/255., 35/255., 101/255.);
		cairo_set_source(cr, scrim);
		cairo_paint(cr);
		cairo_pattern_destroy(scrim);
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
		return x >= BUTTON_X && x < FIELD_X + FIELD_WIDTH
			&& y >= box.field_y && y < box.field_y + FIELD_HEIGHT;
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
