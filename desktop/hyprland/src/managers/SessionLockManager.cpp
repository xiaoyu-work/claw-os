#include "SessionLockManager.hpp"
#include "../Compositor.hpp"
#include "../config/ConfigValue.hpp"
#include "../protocols/FractionalScale.hpp"
#include "../protocols/SessionLock.hpp"
#include "../render/Renderer.hpp"
#include "../desktop/state/FocusState.hpp"
#include "../desktop/view/SessionLock.hpp"
#include "../managers/SeatManager.hpp"
#include "../managers/input/InputManager.hpp"
#include "../state/MonitorState.hpp"
#include <algorithm>
#include <ranges>

SSessionLockSurface::SSessionLockSurface(SP<CSessionLockSurface> surface_) : surface(surface_) {
    pWlrSurface = surface->surface();

    listeners.map = surface_->m_events.map.listen([this] {
        mapped = true;

        g_pInputManager->simulateMouseMovement();

        const auto PMONITOR = State::monitorState()->query().id(iMonitorID).run();

        if (PMONITOR)
            g_pHyprRenderer->damageMonitor(PMONITOR);
    });

    listeners.destroy = surface_->m_events.destroy.listen([this] {
        if (pWlrSurface == Desktop::focusState()->surface())
            Desktop::focusState()->surface().reset();

        g_pSessionLockManager->removeSessionLockSurface(this);
    });

    listeners.commit = surface_->m_events.commit.listen([this] {
        const auto PMONITOR = State::monitorState()->query().id(iMonitorID).run();

        if (mapped && !Desktop::focusState()->surface())
            g_pInputManager->simulateMouseMovement();

        if (PMONITOR)
            g_pHyprRenderer->damageMonitor(PMONITOR);
    });
}

CSessionLockManager::CSessionLockManager() {
    m_listeners.newLock = PROTO::sessionLock->m_events.newLock.listen([this](const auto& lock) { this->onNewSessionLock(lock); });
}

void CSessionLockManager::onNewSessionLock(SP<CSessionLock> pLock) {
    static auto PALLOWRELOCK = CConfigValue<Config::INTEGER>("misc:allow_session_lock_restore");

    if (PROTO::sessionLock->isLocked() && !*PALLOWRELOCK && g_pCompositor->m_startLockedCommand.empty()) {
        LOG(Log::DEBUG, "Cannot re-lock, misc:allow_session_lock_restore is disabled");
        pLock->sendDenied();
        return;
    }

    if (m_sessionLock && !clientDenied() && !clientLocked())
        return; // Not allowing to relock in case the old lock is still in a limbo

    LOG(Log::DEBUG, "Session got locked by {:x}", (uintptr_t)pLock.get());

    m_sessionLock       = makeUnique<SSessionLock>();
    m_sessionLock->lock = pLock;
    m_sessionLock->lockTimer.reset();

    m_sessionLock->listeners.newSurface = pLock->m_events.newLockSurface.listen([this](const SP<CSessionLockSurface>& surface) {
        const auto PMONITOR = surface->monitor();

        const auto NEWSURFACE  = m_sessionLock->vSessionLockSurfaces.emplace_back(makeShared<SSessionLockSurface>(surface));
        NEWSURFACE->iMonitorID = PMONITOR->m_id;
        PROTO::fractional->sendScale(surface->surface(), PMONITOR->m_scale);

        NEWSURFACE->view = Desktop::View::CSessionLock::create(surface);
    });

    m_sessionLock->listeners.unlock = pLock->m_events.unlockAndDestroy.listen([this] {
        m_events.unlock.emit();

        m_sessionLock.reset();
        g_pInputManager->refocus();

        for (auto const& m : State::monitorState()->monitors())
            g_pHyprRenderer->damageMonitor(m);
    });

    m_sessionLock->listeners.destroy = pLock->m_events.destroyed.listen([this] {
        m_sessionLock.reset();
        Desktop::focusState()->rawSurfaceFocus(nullptr);

        for (auto const& m : State::monitorState()->monitors())
            g_pHyprRenderer->damageMonitor(m);
    });

    m_events.lock.emit();

    Desktop::focusState()->rawSurfaceFocus(nullptr);
    g_pSeatManager->setGrab(nullptr);

    for (const auto& monitor : State::monitorState()->monitors())
        g_pHyprRenderer->damageMonitor(monitor);
}

bool CSessionLockManager::isSessionLocked() {
    return PROTO::sessionLock->isLocked();
}

WP<SSessionLockSurface> CSessionLockManager::getSessionLockSurfaceForMonitor(uint64_t id) {
    if (!m_sessionLock)
        return {};

    for (auto const& sls : m_sessionLock->vSessionLockSurfaces) {
        if (sls->iMonitorID == id) {
            if (sls->mapped)
                return sls;
            else
                return {};
        }
    }

    return {};
}

void CSessionLockManager::onLockscreenPresentedOnMonitor(uint64_t id, const WP<CWLSurfaceResource>& surface) {
    if (!m_sessionLock || m_sessionLock->hasSentLocked || m_sessionLock->hasSentDenied || !g_pCompositor->m_sessionActive)
        return;

    const auto CURRENT = getSessionLockSurfaceForMonitor(id);
    if (!CURRENT || !surface || CURRENT->pWlrSurface != surface)
        return;

    m_sessionLock->lockedMonitors.emplace(id);
    const bool LOCKED =
        std::ranges::all_of(State::monitorState()->monitors(), [this](auto m) { return !m->m_enabled || !m->m_dpmsStatus || m_sessionLock->lockedMonitors.contains(m->m_id); });

    if (LOCKED && m_sessionLock->lock->good()) {
        m_sessionLock->lock->sendLocked();
        m_sessionLock->hasSentLocked = true;
    }
}

bool CSessionLockManager::isSurfaceSessionLock(SP<CWLSurfaceResource> pSurface) {
    // TODO: this has some edge cases when it's wrong (e.g. destroyed lock but not yet surfaces)
    // but can be easily fixed when I rewrite wlr_surface

    if (!m_sessionLock)
        return false;

    for (auto const& sls : m_sessionLock->vSessionLockSurfaces) {
        if (sls->surface->surface() == pSurface)
            return true;
    }

    return false;
}

bool CSessionLockManager::anySessionLockSurfacesPresent() {
    return m_sessionLock && std::ranges::any_of(m_sessionLock->vSessionLockSurfaces, [](const auto& surf) { return surf->mapped; });
}

void CSessionLockManager::removeSessionLockSurface(SSessionLockSurface* pSLS) {
    if (!m_sessionLock)
        return;

    std::erase_if(m_sessionLock->vSessionLockSurfaces, [&](const auto& other) { return pSLS == other.get(); });

    if (Desktop::focusState()->surface())
        return;

    for (auto const& sls : m_sessionLock->vSessionLockSurfaces) {
        if (!sls->mapped)
            continue;

        Desktop::focusState()->rawSurfaceFocus(sls->surface->surface());
        break;
    }
}

bool CSessionLockManager::clientLocked() {
    return m_sessionLock && m_sessionLock->hasSentLocked;
}

bool CSessionLockManager::clientDenied() {
    return m_sessionLock && m_sessionLock->hasSentDenied;
}

void CSessionLockManager::forceLock() {
    PROTO::sessionLock->forceLock();
    m_events.lock.emit();
}

bool CSessionLockManager::shallConsiderLockMissing() {
    if (!m_sessionLock)
        return true;

    static auto LOCKDEAD_SCREEN_DELAY = CConfigValue<Config::INTEGER>("misc:lockdead_screen_delay");

    return m_sessionLock->lockTimer.getMillis() > *LOCKDEAD_SCREEN_DELAY;
}
