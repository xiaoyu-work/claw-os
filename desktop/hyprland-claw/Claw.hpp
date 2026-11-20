// SPDX-License-Identifier: GPL-3.0-only
#pragma once

#include "include/claw_compositor.h"
#include <cstdint>
#include <functional>
#include <optional>
#include <string>
#include <string_view>

struct wl_client;
struct wl_display;
struct wl_global;

namespace Claw {
    struct SOrigin {
        uint32_t kind  = 0;
        uint64_t token = 0;
    };

    struct SBackend {
        std::function<void()>         stop;
        std::function<void(uint64_t)> retireContexts;
        // Ownership transfers only when installation succeeds.
        std::function<bool(ClawSelection*)>                                  installSelection;
        std::function<std::optional<std::string>(const ClawDesktopRequest&)> desktop;
    };

    bool initialize();
    bool attach(wl_display* display, SBackend backend);
    bool ready(std::string_view display);
    void shutdown();
    void desktopChanged();
    bool globalAllowed(const wl_client* client, const wl_global* global, bool privileged);
    bool permitted(const wl_client* client, uint32_t right);
    bool contextOrigin(wl_client* client, SOrigin& result);
    bool listenerAdded(SOrigin origin);
    void listenerRemoved(SOrigin origin);
    bool acceptsContext(SOrigin origin, int descriptor);

    // Consumes the descriptor. Allocation failure terminates the display.
    wl_client* createClient(int descriptor, SOrigin origin);
}
