#pragma once

#include "../defines.hpp"
#include "../helpers/time/Timer.hpp"
#include "../helpers/signal/Signal.hpp"
#include "./eventLoop/EventLoopTimer.hpp"
#include <cstdint>
#include <unordered_map>
#include <unordered_set>

class CSessionLockSurface;
class CSessionLock;
class CWLSurfaceResource;

namespace Desktop::View {
    class CSessionLock;
}

struct SSessionLockSurface {
    SSessionLockSurface(SP<CSessionLockSurface> surface_);

    WP<CSessionLockSurface>         surface;
    WP<CWLSurfaceResource>          pWlrSurface;
    SP<Desktop::View::CSessionLock> view;
    uint64_t                        iMonitorID = -1;

    bool                            mapped = false;

    struct {
        CHyprSignalListener map;
        CHyprSignalListener destroy;
        CHyprSignalListener commit;
    } listeners;
};

struct SSessionLock {
    WP<CSessionLock>                     lock;
    CTimer                               lockTimer;

    std::vector<SP<SSessionLockSurface>> vSessionLockSurfaces;

    struct {
        CHyprSignalListener newSurface;
        CHyprSignalListener unlock;
        CHyprSignalListener destroy;
    } listeners;

    bool                         hasSentLocked = false;
    bool                         hasSentDenied = false;
    std::unordered_set<uint64_t> lockedMonitors;
};

class CSessionLockManager {
  public:
    CSessionLockManager();
    ~CSessionLockManager() = default;

    WP<SSessionLockSurface> getSessionLockSurfaceForMonitor(uint64_t);

    bool                    isSessionLocked();
    bool                    clientLocked();
    bool                    clientDenied();
    bool                    isSurfaceSessionLock(SP<CWLSurfaceResource>);
    bool                    anySessionLockSurfacesPresent();

    void                    forceLock();

    void                    removeSessionLockSurface(SSessionLockSurface*);

    void                    onLockscreenPresentedOnMonitor(uint64_t id, const WP<CWLSurfaceResource>& surface);

    bool                    shallConsiderLockMissing();

    struct {
        CSignalT<> lock;
        CSignalT<> unlock;
    } m_events;

  private:
    UP<SSessionLock> m_sessionLock;

    struct {
        CHyprSignalListener newLock;
    } m_listeners;

    void onNewSessionLock(SP<CSessionLock> pWlrLock);
};

inline UP<CSessionLockManager> g_pSessionLockManager;
