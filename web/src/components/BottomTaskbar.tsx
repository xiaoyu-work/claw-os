import { motion } from 'framer-motion';
import { useSystemStore } from '@/stores/useSystemStore';
import { useWindowStore } from '@/stores/useWindowStore';
import { useAppRegistryStore } from '@/stores/useAppRegistryStore';
import AppIcon from '@/components/AppIcon';

const pinnedApps = ['agent', 'filemanager', 'browser', 'texteditor', 'store'];

export default function BottomTaskbar() {
  const activeWorkspace = useSystemStore((s) => s.activeWorkspace);
  const setActiveWorkspace = useSystemStore((s) => s.setActiveWorkspace);
  const windows = useWindowStore((s) => s.windows);
  const openWindow = useWindowStore((s) => s.openWindow);
  const focusWindow = useWindowStore((s) => s.focusWindow);
  const minimizeWindow = useWindowStore((s) => s.minimizeWindow);
  const getApp = useAppRegistryStore((s) => s.getApp);
  const settingsApp = getApp('settings');

  const handleTaskClick = (winId: string) => {
    const win = windows.find((w) => w.id === winId);
    if (!win) return;
    if (win.isMinimized || !win.isFocused) {
      focusWindow(winId);
    } else {
      minimizeWindow(winId);
    }
  };

  const handleOpenApp = (appId: string) => {
    const app = getApp(appId);
    if (!app) return;
    const existing = app.singleton ? windows.find((w) => w.appId === appId && !w.isMinimized) : undefined;
    if (existing) {
      focusWindow(existing.id);
    } else {
      openWindow(appId, app.name, {
        width: app.defaultWidth,
        height: app.defaultHeight,
      });
    }
  };

  const workspaces = [1, 2, 3];

  return (
    <div
      className="fixed bottom-0 left-0 right-0 h-12 flex items-center justify-between px-3 select-none z-40"
      style={{ background: 'rgba(26,26,46,0.95)', backdropFilter: 'blur(16px)', borderTop: '1px solid rgba(255,255,255,0.08)', boxShadow: '0 -2px 12px rgba(0,0,0,0.3)' }}
    >
      {/* Pinned apps */}
      <div className="flex items-center gap-1">
        {pinnedApps.map((appId) => {
          const app = getApp(appId);
          if (!app) return null;
          return (
            <button
              key={app.id}
              onClick={() => handleOpenApp(app.id)}
              className="w-10 h-10 flex items-center justify-center rounded-lg hover:bg-white/10 transition-colors"
              title={app.name}
            >
              <AppIcon icon={app.icon} label={app.name} size={25} className="opacity-80" />
            </button>
          );
        })}
      </div>

      {/* Active tasks */}
      <div className="hidden items-center gap-1 sm:flex">
        {windows.filter((w) => !w.isMinimized).map((win) => {
          const app = getApp(win.appId);
          return (
            <motion.button
              key={win.id}
              layout
              initial={{ scale: 0, opacity: 0 }}
              animate={{ scale: 1, opacity: 1 }}
              exit={{ scale: 0, opacity: 0 }}
              onClick={() => handleTaskClick(win.id)}
              className={`w-10 h-10 flex items-center justify-center rounded-lg transition-colors ${
                win.isFocused ? 'border-b-2 border-[var(--accent-silver)]' : 'border-b-2 border-transparent'
              } hover:bg-white/10`}
            >
              {app && (
                <AppIcon
                  icon={app.icon}
                  label={app.name}
                  size={23}
                  className={win.isFocused ? 'opacity-100' : 'opacity-70'}
                />
              )}
            </motion.button>
          );
        })}
      </div>

      {/* Right controls */}
      <div className="flex items-center gap-2">
        {/* Workspace switcher */}
        <div className="flex items-center gap-1.5 px-2 py-1 rounded-lg" style={{ background: 'rgba(255,255,255,0.10)' }}>
          {workspaces.map((ws) => (
            <button
              key={ws}
              onClick={() => setActiveWorkspace(ws)}
              className="w-5 h-5 rounded-sm transition-all hover:scale-110 flex items-center justify-center text-[10px] font-bold"
              style={{
                background: ws === activeWorkspace ? 'var(--accent-silver)' : 'rgba(255,255,255,0.15)',
                color: ws === activeWorkspace ? '#fff' : 'var(--text-muted)',
                boxShadow: ws === activeWorkspace ? '0 0 6px var(--accent-silver)' : 'none',
              }}
              title={`Workspace ${ws}`}
            >
              {ws}
            </button>
          ))}
        </div>

        <button
          onClick={() => handleOpenApp('settings')}
          className="w-10 h-10 flex items-center justify-center rounded-lg hover:bg-white/10 transition-colors"
          title="Settings"
        >
          {settingsApp && (
            <AppIcon
              icon={settingsApp.icon}
              label={settingsApp.name}
              size={23}
              className="opacity-75"
            />
          )}
        </button>
      </div>
    </div>
  );
}
