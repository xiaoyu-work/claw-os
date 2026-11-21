// SPDX-License-Identifier: GPL-3.0-only

#include "Hyprland.hpp"
#include "../hyprland/src/desktop/state/FocusState.hpp"
#include "../hyprland/src/desktop/view/window/Window.hpp"
#include "../hyprland/src/desktop/view/window/WindowMetadata.hpp"
#include "../hyprland/src/event/EventBus.hpp"
#include "../hyprland/src/managers/SessionLockManager.hpp"
#include "../hyprland/src/managers/input/InputManager.hpp"
#include "../hyprland/src/output/Monitor.hpp"
#include "../hyprland/src/render/OpenGL.hpp"
#include "../hyprland/src/render/Renderer.hpp"
#include "../hyprland/src/render/gl/GLFramebuffer.hpp"
#include <algorithm>
#include <array>
#include <cmath>
#include <ctime>
#include <glaze/json.hpp>
#include <memory>
#include <utility>
#include <vector>

namespace Claw {
    namespace {
        constexpr uint32_t MAX_PICTURE = 640;
        constexpr uint32_t MAX_WINDOW  = 4096;
        constexpr uint64_t FRAME_TTL   = 90'000;

        struct Region {
            uint32_t x, y, width, height;
        };

        struct Frame {
            PHLWINDOWREF         window;
            MONITORID            monitor;
            wl_output_transform  transform;
            double               scale;
            CBox                 geometry;
            Vector2D             nativePixels;
            Region               region;
            std::string          observation;
            uint64_t             expires;
            std::vector<uint8_t> rgba;
        };

        struct Reply {
            bool        ok = false;
            std::string error;
            std::string image;
            uint32_t    width         = 0;
            uint32_t    height        = 0;
            uint32_t    window_width  = 0;
            uint32_t    window_height = 0;
            uint32_t    origin_x      = 0;
            uint32_t    origin_y      = 0;
            bool        clickable     = false;
            bool        clicked       = false;
        };

        std::optional<Frame> current;

        void                 watchFocus() {
            const auto                               changed = [] { current.reset(); };
            auto&                                    events  = Event::bus()->m_events;
            [[maybe_unused]] static const std::array listeners{
                events.window.active.listen(changed),
                events.workspace.active.listen(changed),
                events.monitor.layoutChanged.listen(changed),
            };
        }

        std::string reply(Reply value) {
            if (auto json = glz::write_json(value))
                return *json;
            return R"({"ok":false,"error":"Desktop visual response could not be encoded"})";
        }

        uint64_t nowMs() {
            timespec ts{};
            if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0)
                return UINT64_MAX;
            return static_cast<uint64_t>(ts.tv_sec) * 1000 + ts.tv_nsec / 1'000'000;
        }

        bool available(const PHLWINDOW& window) {
            if (!window)
                return false;
            const auto& app = window->metadata().appID();
            return app != "claw-desktop-reviews" && app != "polkit-mate-authentication-agent-1" && window->mapped() && !window->isHidden() && window->m_ruleApplicator &&
                !window->m_ruleApplicator->noScreenShare().valueOrDefault() && !g_pSessionLockManager->isSessionLocked() && Desktop::focusState()->window() == window &&
                g_pHyprRenderer->shouldRenderWindow(window);
        }

        std::vector<uint8_t> readPixels(const PHLWINDOW& window, const PHLMONITOR& monitor, const Vector2D& nativePixels, Region region, const Vector2D& size) {
            using Render::GL::g_pHyprOpenGL;
            if (g_pHyprRenderer->type() != Render::IHyprRenderer::RT_GL || !g_pHyprOpenGL)
                return {};
            auto frame = dynamicPointerCast<Render::GL::CGLFramebuffer>(g_pHyprRenderer->renderVisualWindow(window, monitor, nativePixels));
            if (!frame)
                return {};
            g_pHyprOpenGL->makeEGLCurrent();
            Render::GL::CFramebufferBindingGuard bindings{g_pHyprOpenGL};
            g_pHyprOpenGL->bindFramebuffer(GL_READ_FRAMEBUFFER, frame->getFBID());
            SP<Render::GL::CGLFramebuffer> preview;
            if (region.width != size.x || region.height != size.y) {
                preview = dynamicPointerCast<Render::GL::CGLFramebuffer>(g_pHyprRenderer->createFB("Claw visual preview"));
                if (!preview || !preview->alloc(size.x, size.y, DRM_FORMAT_ABGR8888))
                    return {};
                g_pHyprOpenGL->bindFramebuffer(GL_DRAW_FRAMEBUFFER, preview->getFBID());
                g_pHyprOpenGL->disableScissor();
                glBlitFramebuffer(region.x, region.y, region.x + region.width, region.y + region.height, 0, 0, size.x, size.y, GL_COLOR_BUFFER_BIT, GL_LINEAR);
                g_pHyprOpenGL->bindFramebuffer(GL_READ_FRAMEBUFFER, preview->getFBID());
                region.x = region.y = 0;
            }
            std::vector<uint8_t> bytes(static_cast<size_t>(size.x * size.y) * 4);
            // RPT_EXPORT uses the same top-first readback as the toplevel screencopy protocol.
            glReadPixels(region.x, region.y, size.x, size.y, GL_RGBA, GL_UNSIGNED_BYTE, bytes.data());
            if (glGetError() != GL_NO_ERROR)
                return {};
            return bytes;
        }

        std::string base64(const std::string& data) {
            constexpr char alphabet[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            std::string    result;
            result.reserve((data.size() + 2) / 3 * 4);
            for (size_t i = 0; i < data.size(); i += 3) {
                const auto a = static_cast<uint8_t>(data[i]);
                const auto b = i + 1 < data.size() ? static_cast<uint8_t>(data[i + 1]) : 0;
                const auto c = i + 2 < data.size() ? static_cast<uint8_t>(data[i + 2]) : 0;
                result.push_back(alphabet[a >> 2]);
                result.push_back(alphabet[((a & 3) << 4) | (b >> 4)]);
                result.push_back(i + 1 < data.size() ? alphabet[((b & 15) << 2) | (c >> 6)] : '=');
                result.push_back(i + 2 < data.size() ? alphabet[c & 63] : '=');
            }
            return result;
        }

        std::string encode(const std::vector<uint8_t>& rgba, int width, int height) {
            std::vector<uint32_t> rgb(static_cast<size_t>(width) * height);
            for (size_t i = 0; i < rgb.size(); ++i)
                rgb[i] = (static_cast<uint32_t>(rgba[4 * i]) << 16) | (static_cast<uint32_t>(rgba[4 * i + 1]) << 8) | rgba[4 * i + 2];

            std::unique_ptr<cairo_surface_t, decltype(&cairo_surface_destroy)> surface{
                cairo_image_surface_create_for_data(reinterpret_cast<uint8_t*>(rgb.data()), CAIRO_FORMAT_RGB24, width, height, width * 4),
                cairo_surface_destroy,
            };
            std::string png;
            const auto  append = [](void* buffer, const unsigned char* data, unsigned int len) {
                auto& bytes = *static_cast<std::string*>(buffer);
                if (len > 1'500'000 - bytes.size())
                    return CAIRO_STATUS_WRITE_ERROR;
                bytes.append(reinterpret_cast<const char*>(data), len);
                return CAIRO_STATUS_SUCCESS;
            };
            if (cairo_surface_write_to_png_stream(surface.get(), append, &png) != CAIRO_STATUS_SUCCESS)
                return {};
            return base64(png);
        }
    }

    bool visualReady(const PHLWINDOW& window) {
        return g_pHyprRenderer && g_pHyprRenderer->type() == Render::IHyprRenderer::RT_GL && available(window);
    }

    std::string desktopVisual(const ClawDesktopRequest& request, const PHLWINDOW& window) {
        watchFocus();
        if (!visualReady(window))
            return reply({.error = "The selected window is not active or permits no visual access"});
        const auto monitor  = window->m_monitor.lock();
        const auto geometry = window->getWindowMainSurfaceBox();
        if (!monitor || !monitor->m_output || monitor->isMirror() || monitor->m_cursorZoom->value() != 1.F ||
            geometry != window->geometricBox(Desktop::View::IGeometric::GEOMETRIC_GOAL) || !std::isfinite(monitor->m_scale) || monitor->m_scale <= 0 ||
            !std::isfinite(geometry.x) || !std::isfinite(geometry.y) || !std::isfinite(geometry.w) || !std::isfinite(geometry.h))
            return reply({.error = "The selected window has no usable display"});

        const auto nativePixels = (geometry.size() * monitor->m_scale).round();
        if (nativePixels.x < 1 || nativePixels.y < 1 || nativePixels.x > MAX_WINDOW || nativePixels.y > MAX_WINDOW)
            return reply({.error = "The selected window exceeds the visual frame bounds"});
        const auto observation = std::string(reinterpret_cast<const char*>(request.observation));

        if (request.kind == CLAW_DESKTOP_VISUAL_CLICK) {
            if (!current || current->observation != observation)
                return reply({.error = "The observation was replaced or already used; observe the window again"});
            auto       snapshot = std::exchange(current, std::nullopt);
            const auto region   = snapshot->region;
            if (nowMs() >= snapshot->expires || snapshot->window != window || snapshot->geometry != geometry || snapshot->monitor != monitor->m_id ||
                snapshot->transform != monitor->m_transform || snapshot->scale != monitor->m_scale || snapshot->nativePixels != nativePixels || request.x >= region.width ||
                request.y >= region.height)
                return reply({.error = "The window, output or observation changed; observe again before clicking"});
            const Vector2D cropSize{static_cast<int>(region.width), static_cast<int>(region.height)};
            if (readPixels(window, monitor, nativePixels, region, cropSize) != snapshot->rgba)
                return reply({.error = "The selected pixels changed; observe again before clicking"});
            const Vector2D point{
                geometry.x + (region.x + request.x + .5) / monitor->m_scale,
                geometry.y + (region.y + request.y + .5) / monitor->m_scale,
            };
            if (nowMs() >= request.expires_ms || !available(window) || !geometry.containsPoint(point) || !monitor->logicalBox().containsPoint(point) ||
                !g_pInputManager->clickVisualWindow(window, point))
                return reply({.error = "The point is not an unobstructed input target in the observed window; no click was sent"});
            return reply({.ok = true, .clicked = true});
        }

        current.reset();
        const auto   width  = static_cast<uint32_t>(nativePixels.x);
        const auto   height = static_cast<uint32_t>(nativePixels.y);
        const Region region = request.width ? Region{request.x, request.y, request.width, request.height} : Region{0, 0, width, height};
        if (!region.width || !region.height || region.width > width || region.height > height || region.x > width - region.width || region.y > height - region.height)
            return reply({.error = "The requested crop lies outside the window"});

        const auto     longest = std::max(region.width, region.height);
        const auto     factor  = std::min(1.0, static_cast<double>(MAX_PICTURE) / longest);
        const Vector2D size{std::max(1.0, std::round(region.width * factor)), std::max(1.0, std::round(region.height * factor))};
        auto           pixels = readPixels(window, monitor, nativePixels, region, size);
        if (pixels.empty())
            return reply({.error = "The compositor could not read this window"});
        auto image = encode(pixels, size.x, size.y);
        if (image.empty() || nowMs() >= request.expires_ms)
            return reply({.error = "The window image did not fit the capture budget"});
        const bool clickable = longest <= MAX_PICTURE;
        if (clickable)
            current = Frame{window, monitor->m_id, monitor->m_transform, monitor->m_scale, geometry, nativePixels, region, observation, nowMs() + FRAME_TTL, std::move(pixels)};
        return reply({.ok            = true,
                      .image         = std::move(image),
                      .width         = static_cast<uint32_t>(size.x),
                      .height        = static_cast<uint32_t>(size.y),
                      .window_width  = width,
                      .window_height = height,
                      .origin_x      = region.x,
                      .origin_y      = region.y,
                      .clickable     = clickable});
    }
}
