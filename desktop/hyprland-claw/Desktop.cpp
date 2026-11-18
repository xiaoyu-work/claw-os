// SPDX-License-Identifier: GPL-3.0-only

#include "Hyprland.hpp"
#include "../hyprland/src/config/shared/actions/ConfigActions.hpp"
#include "../hyprland/src/desktop/state/FocusState.hpp"
#include "../hyprland/src/desktop/state/WindowState.hpp"
#include "../hyprland/src/desktop/view/window/Window.hpp"
#include "../hyprland/src/desktop/view/window/WindowMetadata.hpp"
#include "../hyprland/src/devices/IKeyboard.hpp"
#include "../hyprland/src/managers/fullscreen/FullscreenController.hpp"
#include "../hyprland/src/managers/input/InputManager.hpp"
#include "../hyprland/src/output/Monitor.hpp"
#include "../hyprland/src/state/MonitorState.hpp"
#include "../hyprland/src/state/WorkspaceState.hpp"
#include <algorithm>
#include <glaze/json.hpp>
#include <sstream>
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
        std::string              workspace;
        int64_t                  monitor;
        bool                     floating;
        bool                     xwayland;
        bool                     manageable = true;
    };

    struct SWorkspace {
        std::string             selector;
        std::string             name;
        std::optional<uint32_t> number;
        int64_t                 monitor;
        bool                    urgent;
    };

    struct SOutput {
        int64_t     id;
        std::string name;
        std::string workspace;
        std::string special;
        bool        focused;
        double      width;
        double      height;
        double      refresh;
        double      x;
        double      y;
        double      scale;
    };

    struct SKeyboard {
        std::string layout;
        std::string default_layout;
        std::string active_name;
        bool        caps_lock;
        bool        num_lock;
    };

    struct SDesktopResponse {
        bool                     ok = true;
        std::string              error;
        std::vector<SWindow>     windows;
        size_t                   count     = 0;
        size_t                   requested = 0;
        std::optional<SWindow>   before;
        std::vector<SWorkspace>  workspaces;
        std::vector<SOutput>     outputs;
        std::optional<SKeyboard> keyboard;
    };

    static std::string selector(const PHLWORKSPACE& workspace) {
        if (!workspace)
            return {};
        if (workspace->type() == Workspace::eWorkspaceType::NORMAL && !workspace->numberedID())
            return "name:" + workspace->addressableName();
        return workspace->addressableName();
    }

    static bool managed(const PHLWINDOW& window) {
        return window && window->mapped() && !window->backend().traits().overrideRedirect;
    }

    static std::optional<SKeyboard> currentKeyboard() {
        for (const auto& keyboard : g_pInputManager->m_keyboards) {
            if (!keyboard->m_active || !keyboard->m_xkbState)
                continue;
            std::istringstream layouts(keyboard->m_currentRules.layout);
            SKeyboard          result{
                .layout         = {},
                .default_layout = {},
                .active_name    = keyboard->getActiveLayout(),
                .caps_lock      = xkb_state_mod_name_is_active(keyboard->m_xkbState, XKB_MOD_NAME_CAPS, XKB_STATE_MODS_LOCKED) > 0,
                .num_lock       = xkb_state_mod_name_is_active(keyboard->m_xkbState, XKB_MOD_NAME_NUM, XKB_STATE_MODS_LOCKED) > 0,
            };
            const auto index   = keyboard->getActiveLayoutIndex().value_or(0);
            size_t     current = 0;
            for (std::string layout; std::getline(layouts, layout, ',');) {
                if (result.default_layout.empty())
                    result.default_layout = layout;
                if (current++ == index) {
                    result.layout = layout;
                    break;
                }
            }
            return result;
        }
        return std::nullopt;
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
            .workspace       = selector(window->m_workspace),
            .monitor         = window->monitorID(),
            .floating        = window->isFloating(),
            .xwayland        = window->backend().isX11(),
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
        namespace Action = Config::Actions;
        SDesktopResponse  result;
        const auto        windows = Desktop::windowState()->windows();
        const std::string workspace(reinterpret_cast<const char*>(request.workspace));
        auto              applied = [&](const Action::ActionResult& action) {
            result.ok = action.has_value();
            if (result.ok)
                result.requested = 1;
            else
                result.error = "compositor refused the requested desktop action";
        };
        if (request.kind == CLAW_DESKTOP_LIST) {
            result.keyboard = currentKeyboard();
            for (const auto& ws : State::workspaceState()->workspacesCopy())
                result.workspaces.push_back({selector(ws), ws->displayName(), ws->numberedID(), ws->monitorID(), ws->hasUrgentWindow()});
            for (const auto& monitor : State::monitorState()->monitors()) {
                result.outputs.push_back({monitor->m_id, monitor->m_name, selector(monitor->m_activeWorkspace), selector(monitor->m_activeSpecialWorkspace),
                                          Desktop::focusState()->monitor() == monitor, monitor->m_pixelSize.x, monitor->m_pixelSize.y,
                                          monitor->m_refreshRate, monitor->m_position.x, monitor->m_position.y, monitor->m_scale});
            }
        } else if (request.kind == CLAW_DESKTOP_FOCUS_WORKSPACE) {
            applied(Action::changeWorkspace(workspace));
        } else if (request.kind == CLAW_DESKTOP_TOGGLE_SPECIAL) {
            const auto workspaces = State::workspaceState()->workspacesCopy();
            const auto special    = std::ranges::find_if(workspaces, [&](const auto& ws) { return selector(ws) == workspace; });
            applied(Action::toggleSpecial(special == workspaces.end() ? nullptr : *special));
        } else {
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
                    case CLAW_DESKTOP_MOVE_WINDOW: applied(Action::moveToWorkspace(workspace, false, window)); break;
                    case CLAW_DESKTOP_TOGGLE_FLOATING: applied(Action::floatWindow(Action::TOGGLE_ACTION_TOGGLE, window)); break;
                    case CLAW_DESKTOP_TOGGLE_PIN: applied(Action::pinWindow(Action::TOGGLE_ACTION_TOGGLE, window)); break;
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
