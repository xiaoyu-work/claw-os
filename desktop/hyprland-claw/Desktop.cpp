// SPDX-License-Identifier: GPL-3.0-only

#include "Hyprland.hpp"
#include "../hyprland/src/desktop/state/FocusState.hpp"
#include "../hyprland/src/desktop/state/WindowState.hpp"
#include "../hyprland/src/desktop/view/window/Window.hpp"
#include "../hyprland/src/desktop/view/window/WindowMetadata.hpp"
#include "../hyprland/src/managers/fullscreen/FullscreenController.hpp"
#include "../hyprland/src/managers/input/InputManager.hpp"
#include <algorithm>
#include <glaze/json.hpp>
#include <vector>

namespace Claw {
    struct SGeometry {
        double x;
        double y;
        double width;
        double height;
    };

    struct SWindow {
        uint64_t                 id;
        std::string              app_id;
        std::string              title;
        std::vector<std::string> states;
        size_t                   output_count;
        size_t                   workspace_count;
        std::vector<SGeometry>   geometry;
        bool                     manageable = true;
    };

    struct SDesktopResponse {
        bool                   ok = true;
        std::string            error;
        std::vector<SWindow>   windows;
        size_t                 count     = 0;
        size_t                 requested = 0;
        std::optional<SWindow> before;
    };

    static bool managed(const PHLWINDOW& window) {
        return window && window->mapped() && !window->backend().traits().overrideRedirect;
    }

    static SWindow describe(const PHLWINDOW& window) {
        const auto box = window->getWindowMainSurfaceBox();
        SWindow    result{
            .id              = window->metadata().stableID(),
            .app_id          = window->metadata().appID(),
            .title           = window->metadata().title(),
            .output_count    = window->m_monitor.expired() ? 0U : 1U,
            .workspace_count = window->m_workspace ? 1U : 0U,
            .geometry        = {{box.x, box.y, box.w, box.h}},
        };
        if (Desktop::focusState()->isWindowActive(window))
            result.states.emplace_back("activated");
        if (Fullscreen::controller()->isFullscreen(window)) {
            result.states.emplace_back(Fullscreen::controller()->getFullscreenModes(window).internal == Fullscreen::FSMODE_FULLSCREEN ? "fullscreen" : "maximized");
        }
        if (window->m_state & Desktop::View::WINDOW_STATE_PINNED)
            result.states.emplace_back("sticky");
        std::ranges::sort(result.states);
        return result;
    }

    std::optional<std::string> desktop(const ClawDesktopRequest& request) {
        SDesktopResponse result;
        const auto       windows = Desktop::windowState()->windows();
        if (request.kind != CLAW_DESKTOP_LIST) {
            const auto selected = std::ranges::find_if(windows, [&](const auto& window) { return managed(window) && window->metadata().stableID() == request.identifier; });
            if (selected == windows.end()) {
                result.ok    = false;
                result.error = "desktop window is unavailable";
            } else {
                const auto window = *selected;
                result.before     = describe(window);
                switch (request.kind) {
                    case CLAW_DESKTOP_FOCUS:
                        window->activate(true);
                        g_pInputManager->simulateMouseMovement();
                        result.requested = 1;
                        break;
                    case CLAW_DESKTOP_CLOSE:
                        window->sendClose();
                        result.requested = 1;
                        break;
                    case CLAW_DESKTOP_CLOSE_APP: {
                        const std::string appID(reinterpret_cast<const char*>(request.app_id));
                        if (window->metadata().appID() != appID) {
                            result.ok    = false;
                            result.error = "selected window AppID changed";
                            break;
                        }
                        for (const auto& candidate : windows) {
                            if (managed(candidate) && candidate->metadata().appID() == appID) {
                                candidate->sendClose();
                                ++result.requested;
                            }
                        }
                        break;
                    }
                    default: return std::nullopt;
                }
            }
        }
        for (const auto& window : windows) {
            if (managed(window))
                result.windows.push_back(describe(window));
        }
        result.count       = result.windows.size();
        const auto encoded = glz::write_json(result);
        if (!encoded)
            return std::nullopt;
        return *encoded;
    }
}
