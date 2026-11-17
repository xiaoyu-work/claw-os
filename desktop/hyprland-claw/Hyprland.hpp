// SPDX-License-Identifier: GPL-3.0-only
#pragma once

#include "include/claw_compositor.h"
#include <optional>
#include <string>

namespace Claw {
    bool                       installSelection(ClawSelection* selection);
    void                       clearSelection();
    std::optional<std::string> desktop(const ClawDesktopRequest& request);
}
