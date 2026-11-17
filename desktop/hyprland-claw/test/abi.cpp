// SPDX-License-Identifier: GPL-3.0-only

#include "../include/claw_compositor.h"
#include "../Claw.hpp"
#include <cstdlib>
#include <iostream>

int main() {
    if (::unsetenv("CLAW_DISPLAY_CONTROL_FD") != 0)
        return 1;
    if (claw_compositor_open() != nullptr)
        return 2;
    ClawCommand command{};
    if (claw_compositor_next(nullptr, &command) != -1)
        return 3;
    if (claw_compositor_rights(nullptr, CLAW_ORIGIN_INSTANCE, 1) != 0)
        return 4;
    if (claw_compositor_context_created(nullptr, CLAW_ORIGIN_CREATOR, 1))
        return 5;
    if (claw_compositor_take_descriptor(nullptr, 1) != -1)
        return 6;
    if (claw_compositor_take_selection(nullptr, 1) != nullptr)
        return 7;
    if (claw_selection_count(nullptr) != 0)
        return 8;
    if (claw_selection_send(nullptr, nullptr, 0, -1))
        return 9;
    if (Claw::initialize())
        return 10;
    if (Claw::permitted(nullptr, CLAW_PANEL))
        return 11;
    Claw::SOrigin origin{};
    if (Claw::contextOrigin(nullptr, origin))
        return 12;
    ClawDesktopRequest desktop{};
    if (claw_compositor_take_desktop(nullptr, 1, &desktop))
        return 13;
    if (claw_compositor_desktop_response(nullptr, 1, nullptr, 0))
        return 14;
    Claw::shutdown();
    std::cout << "Native compositor ABI rejects unauthenticated startup and invalid handles\n";
    return 0;
}
