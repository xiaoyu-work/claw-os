// SPDX-License-Identifier: GPL-3.0-only
#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct ClawCompositor ClawCompositor;
typedef struct ClawSelection  ClawSelection;

enum ClawOriginKind {
    CLAW_ORIGIN_LOGIN    = 1,
    CLAW_ORIGIN_CREATOR  = 2,
    CLAW_ORIGIN_INSTANCE = 3,
};

enum ClawRights {
    CLAW_SELECTION_READ  = 1,
    CLAW_SELECTION_WRITE = 2,
    CLAW_PANEL           = 4,
    CLAW_CONTEXT         = 8,
    CLAW_OS_SESSION      = 16,
};

enum ClawCommandKind {
    CLAW_CREATE         = 1,
    CLAW_RETIRE         = 2,
    CLAW_SET_SELECTION  = 3,
    CLAW_STOP           = 4,
    CLAW_INSTANCE_ENDED = 5,
    CLAW_DESKTOP        = 6,
};

enum ClawDesktopKind {
    CLAW_DESKTOP_LIST      = 1,
    CLAW_DESKTOP_FOCUS     = 2,
    CLAW_DESKTOP_CLOSE     = 3,
    CLAW_DESKTOP_CLOSE_APP = 4,
};

typedef struct ClawDesktopRequest {
    uint32_t kind;
    uint64_t identifier;
    uint8_t  app_id[256];
} ClawDesktopRequest;

typedef struct ClawCommand {
    uint64_t serial;
    uint64_t token;
    uint32_t kind;
} ClawCommand;

/*
 * OS-private ABI, not an App interface. Handles and commands are confined to
 * the compositor event thread. Only this library may create handles/tokens.
 * All non-null pointers must remain valid for the duration of a call.
 * Creator descriptors and selection handles transfer ownership once; other
 * descriptor parameters are borrowed. MIME pointers live with their selection.
 * False, zero rights or a negative status never grants authority.
 */
ClawCompositor* claw_compositor_open(void);
void            claw_compositor_close(ClawCompositor* handle);
bool            claw_compositor_ready(ClawCompositor* handle, const uint8_t* display, size_t length);
int32_t         claw_compositor_next(ClawCompositor* handle, ClawCommand* command);
bool            claw_compositor_acknowledge(ClawCompositor* handle, uint64_t serial, bool accepted);
int32_t         claw_compositor_take_descriptor(ClawCompositor* handle, uint64_t serial);
bool            claw_compositor_take_desktop(ClawCompositor* handle, uint64_t serial, ClawDesktopRequest* request);
bool            claw_compositor_desktop_response(ClawCompositor* handle, uint64_t serial, const uint8_t* bytes, size_t length);
uint32_t        claw_compositor_rights(ClawCompositor* handle, uint32_t origin, uint64_t token);
bool            claw_compositor_accepts_login(ClawCompositor* handle, int32_t descriptor);
bool            claw_compositor_client_added(ClawCompositor* handle, uint32_t origin, uint64_t token, int32_t descriptor);
bool            claw_compositor_client_removed(ClawCompositor* handle, uint32_t origin, uint64_t token);
bool            claw_compositor_context_created(ClawCompositor* handle, uint32_t origin, uint64_t token);
bool            claw_compositor_listener_added(ClawCompositor* handle, uint64_t token);
bool            claw_compositor_listener_removed(ClawCompositor* handle, uint64_t token);
bool            claw_compositor_accepts_context(ClawCompositor* handle, uint64_t token, int32_t descriptor);
ClawSelection*  claw_compositor_take_selection(ClawCompositor* handle, uint64_t serial);
size_t          claw_selection_count(const ClawSelection* selection);
const char*     claw_selection_mime(const ClawSelection* selection, size_t index);
bool            claw_selection_send(const ClawSelection* selection, const uint8_t* mime, size_t length, int32_t descriptor);
void            claw_selection_close(ClawSelection* selection);

#ifdef __cplusplus
}
#endif
