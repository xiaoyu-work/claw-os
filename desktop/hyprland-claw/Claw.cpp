// SPDX-License-Identifier: GPL-3.0-only

#include "Claw.hpp"
#include <algorithm>
#include <iostream>
#include <memory>
#include <optional>
#include <string_view>
#include <unordered_map>
#include <utility>
#include <vector>
#include <unistd.h>
#include <wayland-server-core.h>

namespace Claw {
    class CController;

    struct SClient {
        wl_listener  destroy{};
        CController* owner  = nullptr;
        wl_client*   client = nullptr;
        SOrigin      origin{};
        bool         admitted = false;
    };

    struct SDisplayWatch {
        wl_listener  created{};
        CController* owner = nullptr;
    };

    class CController {
      public:
        CController();
        ~CController();

        bool       good() const;
        bool       attach(wl_display* display, SBackend backend);
        bool       ready(std::string_view display);
        void       detach();
        uint32_t   rights(const wl_client* client) const;
        bool       claimContext(wl_client* client, SOrigin& result);
        bool       addListener(SOrigin origin);
        void       removeListener(SOrigin origin);
        bool       accepts(SOrigin origin, int descriptor) const;
        wl_client* createClient(int descriptor, SOrigin origin);
        bool       known(const wl_client* client) const;

      private:
        ClawCompositor*                                                m_handle  = nullptr;
        wl_display*                                                    m_display = nullptr;
        wl_event_source*                                               m_timer   = nullptr;
        SDisplayWatch                                                  m_watch{};
        SBackend                                                       m_backend{};
        bool                                                           m_stopping = false;
        std::optional<SOrigin>                                         m_pendingOrigin;
        std::unordered_map<const wl_client*, std::unique_ptr<SClient>> m_clients;

        static void                                                    clientCreated(wl_listener* listener, void* data);
        static void                                                    clientDestroyed(wl_listener* listener, void* data);
        static int                                                     tick(void* data);
        void                                                           registerClient(wl_client* client);
        void                                                           removeClient(SClient* client);
        void                                                           retire(uint64_t token);
        void                                                           retireAll();
        void                                                           dispatch();
        void                                                           fail();
    };

    static std::unique_ptr<CController>& controller() {
        static std::unique_ptr<CController> instance;
        return instance;
    }

    CController::CController() : m_handle(claw_compositor_open()) {
        wl_list_init(&m_watch.created.link);
    }

    CController::~CController() {
        detach();
        if (m_handle)
            claw_compositor_close(m_handle);
    }

    bool CController::good() const {
        return m_handle != nullptr;
    }

    bool CController::attach(wl_display* display, SBackend backend) {
        if (!m_handle || !display || m_display || !backend.stop || !backend.retireContexts || !backend.installSelection || !backend.desktop)
            return false;
        m_display              = display;
        m_backend              = std::move(backend);
        m_watch.owner          = this;
        m_watch.created.notify = clientCreated;
        wl_display_add_client_created_listener(display, &m_watch.created);
        m_timer = wl_event_loop_add_timer(wl_display_get_event_loop(display), tick, this);
        if (!m_timer || wl_event_source_timer_update(m_timer, 10) < 0) {
            detach();
            return false;
        }
        return true;
    }

    bool CController::ready(std::string_view display) {
        return m_handle && !m_stopping && claw_compositor_ready(m_handle, reinterpret_cast<const uint8_t*>(display.data()), display.size());
    }

    void CController::detach() {
        m_stopping = true;
        if (m_timer) {
            wl_event_source_remove(m_timer);
            m_timer = nullptr;
        }
        if (m_display) {
            if (m_backend.retireContexts)
                m_backend.retireContexts(0);
            while (!m_clients.empty())
                wl_client_destroy(m_clients.begin()->second->client);
            wl_list_remove(&m_watch.created.link);
            wl_list_init(&m_watch.created.link);
            m_display = nullptr;
        }
    }

    void CController::clientCreated(wl_listener* listener, void* data) {
        auto* watch = reinterpret_cast<SDisplayWatch*>(listener);
        watch->owner->registerClient(static_cast<wl_client*>(data));
    }

    void CController::clientDestroyed(wl_listener* listener, void*) {
        auto* client = reinterpret_cast<SClient*>(listener);
        client->owner->removeClient(client);
    }

    void CController::registerClient(wl_client* client) {
        const auto origin      = std::exchange(m_pendingOrigin, std::nullopt).value_or(SOrigin{CLAW_ORIGIN_LOGIN, 0});
        auto       record      = std::make_unique<SClient>();
        record->owner          = this;
        record->client         = client;
        record->origin         = origin;
        record->admitted       = !m_stopping && claw_compositor_client_added(m_handle, origin.kind, origin.token, wl_client_get_fd(client));
        record->destroy.notify = clientDestroyed;
        wl_client_add_destroy_listener(client, &record->destroy);
        const bool admitted = record->admitted;
        m_clients.emplace(client, std::move(record));
        if (!admitted)
            wl_client_post_implementation_error(client, "Claw display connection has no live authenticated origin");
    }

    void CController::removeClient(SClient* client) {
        const auto found = m_clients.find(client->client);
        if (found == m_clients.end())
            return;
        const bool admitted = client->admitted;
        const auto origin   = client->origin;
        wl_list_remove(&client->destroy.link);
        m_clients.erase(found);
        if (admitted && !claw_compositor_client_removed(m_handle, origin.kind, origin.token))
            fail();
    }

    bool CController::known(const wl_client* client) const {
        const auto found = m_clients.find(client);
        return found != m_clients.end() && found->second->admitted;
    }

    uint32_t CController::rights(const wl_client* client) const {
        const auto found = m_clients.find(client);
        if (!m_handle || m_stopping || found == m_clients.end() || !found->second->admitted)
            return 0;
        const auto origin = found->second->origin;
        return claw_compositor_rights(m_handle, origin.kind, origin.token);
    }

    bool CController::claimContext(wl_client* client, SOrigin& result) {
        const auto found = m_clients.find(client);
        if (!m_handle || m_stopping || found == m_clients.end() || !found->second->admitted)
            return false;
        const auto origin = found->second->origin;
        if (!claw_compositor_context_created(m_handle, origin.kind, origin.token))
            return false;
        result = {origin.kind == CLAW_ORIGIN_LOGIN ? CLAW_ORIGIN_LOGIN : CLAW_ORIGIN_INSTANCE, origin.token};
        return true;
    }

    bool CController::addListener(SOrigin origin) {
        return m_handle && !m_stopping && claw_compositor_listener_added(m_handle, origin.token);
    }

    void CController::removeListener(SOrigin origin) {
        if (m_handle && !claw_compositor_listener_removed(m_handle, origin.token))
            fail();
    }

    bool CController::accepts(SOrigin origin, int descriptor) const {
        return m_handle && !m_stopping && claw_compositor_accepts_context(m_handle, origin.token, descriptor);
    }

    wl_client* CController::createClient(int descriptor, SOrigin origin) {
        if (!m_display || m_stopping || m_pendingOrigin || descriptor < 0) {
            if (descriptor >= 0)
                close(descriptor);
            return nullptr;
        }
        m_pendingOrigin = origin;
        auto* client    = wl_client_create(m_display, descriptor);
        m_pendingOrigin.reset();
        if (!client) {
            fail();
            return nullptr;
        }
        if (!known(client)) {
            wl_client_destroy(client);
            return nullptr;
        }
        return client;
    }

    void CController::retire(uint64_t token) {
        if (token == 0)
            return;
        m_backend.retireContexts(token);
        std::vector<wl_client*> clients;
        for (const auto& [client, record] : m_clients) {
            if (record->admitted && record->origin.token == token)
                clients.emplace_back(record->client);
        }
        for (auto* client : clients)
            wl_client_destroy(client);
    }

    void CController::retireAll() {
        m_backend.retireContexts(0);
        while (!m_clients.empty())
            wl_client_destroy(m_clients.begin()->second->client);
    }

    void CController::fail() {
        if (m_stopping)
            return;
        m_stopping = true;
        std::cerr << "Claw compositor authority unavailable; stopping display\n";
        if (m_backend.stop)
            m_backend.stop();
    }

    int CController::tick(void* data) {
        auto* self = static_cast<CController*>(data);
        self->dispatch();
        if (!self->m_stopping && self->m_timer)
            wl_event_source_timer_update(self->m_timer, 10);
        return 0;
    }

    void CController::dispatch() {
        if (m_stopping)
            return;
        for (size_t count = 0; count < 16; ++count) {
            ClawCommand command{};
            const auto  status = claw_compositor_next(m_handle, &command);
            if (status < 0) {
                fail();
                return;
            }
            if (status == 0)
                return;
            bool accepted = false;
            switch (command.kind) {
                case CLAW_CREATE: {
                    const auto descriptor = claw_compositor_take_descriptor(m_handle, command.serial);
                    accepted              = descriptor >= 0 && createClient(descriptor, {CLAW_ORIGIN_CREATOR, command.token});
                    break;
                }
                case CLAW_LOCKER: {
                    const auto descriptor = claw_compositor_take_descriptor(m_handle, command.serial);
                    accepted              = descriptor >= 0 && createClient(descriptor, {CLAW_ORIGIN_LOCKER, 0});
                    break;
                }
                case CLAW_RETIRE:
                case CLAW_INSTANCE_ENDED:
                    retire(command.token);
                    accepted = true;
                    break;
                case CLAW_SET_SELECTION: {
                    auto* selection = claw_compositor_take_selection(m_handle, command.serial);
                    accepted        = selection && m_backend.installSelection(selection);
                    if (selection && !accepted)
                        claw_selection_close(selection);
                    break;
                }
                case CLAW_DESKTOP: {
                    ClawDesktopRequest request{};
                    if (claw_compositor_take_desktop(m_handle, command.serial, &request)) {
                        const auto response = m_backend.desktop(request);
                        accepted = response && claw_compositor_desktop_response(m_handle, command.serial, reinterpret_cast<const uint8_t*>(response->data()), response->size());
                    }
                    break;
                }
                case CLAW_STOP:
                    retireAll();
                    accepted = true;
                    break;
                default: fail(); return;
            }
            if (!claw_compositor_acknowledge(m_handle, command.serial, accepted)) {
                fail();
                return;
            }
            if (command.kind == CLAW_STOP) {
                m_stopping = true;
                m_backend.stop();
                return;
            }
        }
    }

    bool initialize() {
        if (controller())
            return false;
        auto instance = std::make_unique<CController>();
        if (!instance->good())
            return false;
        controller() = std::move(instance);
        return true;
    }

    bool attach(wl_display* display, SBackend backend) {
        return controller() && controller()->attach(display, std::move(backend));
    }

    bool ready(std::string_view display) {
        return controller() && controller()->ready(display);
    }

    void shutdown() {
        if (controller())
            controller()->detach();
        controller().reset();
    }

    bool permitted(const wl_client* client, uint32_t right) {
        return right != 0 && controller() && (controller()->rights(client) & right) == right;
    }

    bool globalAllowed(const wl_client* client, const wl_global* global, bool privileged) {
        if (!controller() || !controller()->known(client))
            return false;
        const auto interface = std::string_view(wl_global_get_interface(global)->name);
        const auto rights    = controller()->rights(client);
        if (interface == "ext_session_lock_manager_v1")
            return rights & CLAW_SESSION_LOCK;
        if (interface == "wp_security_context_manager_v1")
            return rights & CLAW_CONTEXT;
        if (interface == "zwlr_layer_shell_v1" || interface == "hyprland_focus_grab_manager_v1")
            return rights & CLAW_PANEL;
        if (interface == "zwlr_data_control_manager_v1" || interface == "ext_data_control_manager_v1")
            return rights & (CLAW_SELECTION_READ | CLAW_SELECTION_WRITE);
        return !privileged || (rights & CLAW_OS_SESSION);
    }

    bool contextOrigin(wl_client* client, SOrigin& result) {
        return controller() && controller()->claimContext(client, result);
    }

    bool listenerAdded(SOrigin origin) {
        return controller() && controller()->addListener(origin);
    }

    void listenerRemoved(SOrigin origin) {
        if (controller())
            controller()->removeListener(origin);
    }

    bool acceptsContext(SOrigin origin, int descriptor) {
        return controller() && controller()->accepts(origin, descriptor);
    }

    wl_client* createClient(int descriptor, SOrigin origin) {
        if (!controller()) {
            if (descriptor >= 0)
                close(descriptor);
            return nullptr;
        }
        return controller()->createClient(descriptor, origin);
    }
}
