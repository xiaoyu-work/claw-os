import { useState } from 'react';
import { motion, AnimatePresence } from 'framer-motion';
import { FolderOpen, Globe, Terminal, FileText, Settings, Power, LayoutGrid } from 'lucide-react';
import { useSystemStore } from '@/stores/useSystemStore';
import { useWindowStore } from '@/stores/useWindowStore';
import { useAppRegistryStore } from '@/stores/useAppRegistryStore';
import { useSettingsStore } from '@/stores/useSettingsStore';
import * as Icons from 'lucide-react';

interface BottomTaskbarProps {
  onOpenAppMenu: () => void;
}

const pinnedApps = [
  { id: 'filemanager', icon: FolderOpen },
  { id: 'browser', icon: Globe },
  { id: 'terminal', icon: Terminal },
  { id: 'texteditor', icon: FileText },
  { id: 'settings', icon: Settings },
];

export default function BottomTaskbar({ onOpenAppMenu }: BottomTaskbarProps) {
  const activeWorkspace = useSystemStore((s) => s.activeWorkspace);
  const setActiveWorkspace = useSystemStore((s) => s.setActiveWorkspace);
  const windows = useWindowStore((s) => s.windows);
  const openWindow = useWindowStore((s) => s.openWindow);
  const focusWindow = useWindowStore((s) => s.focusWindow);
  const minimizeWindow = useWindowStore((s) => s.minimizeWindow);
  const getApp = useAppRegistryStore((s) => s.getApp);
  const [showPower, setShowPower] = useState(false);

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
        {pinnedApps.map((app) => (
          <button
            key={app.id}
            onClick={() => handleOpenApp(app.id)}
            className="w-10 h-10 flex items-center justify-center rounded-lg hover:bg-white/10 transition-colors"
            title={getApp(app.id)?.name}
          >
            <app.icon size={24} className="text-white/70" />
          </button>
        ))}
      </div>

      {/* Active tasks */}
      <div className="flex items-center gap-1">
        {windows.filter((w) => !w.isMinimized).map((win) => {
          const app = getApp(win.appId);
          const IconComp = app?.icon ? (Icons as unknown as Record<string, React.ComponentType<{ size?: number; className?: string }>>)[app.icon] : null;
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
              {IconComp && <IconComp size={22} className={win.isFocused ? 'text-white' : 'text-white/70'} />}
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
        >
          <Settings size={20} className="text-white/70" />
        </button>

        <div className="relative">
          <button
            onClick={() => setShowPower(!showPower)}
            className="w-10 h-10 flex items-center justify-center rounded-lg hover:bg-white/10 transition-colors"
          >
            <Power size={20} className="text-white/70" />
          </button>

          <AnimatePresence>
            {showPower && (
              <motion.div
                initial={{ scale: 0.95, opacity: 0 }}
                animate={{ scale: 1, opacity: 1 }}
                exit={{ scale: 0.95, opacity: 0 }}
                className="absolute bottom-14 right-0 w-64 rounded-xl p-4 z-50"
                style={{ background: 'var(--bg-panel)', boxShadow: '0 8px 32px rgba(0,0,0,0.5)', border: '1px solid rgba(255,255,255,0.08)' }}
              >
                <h3 className="text-lg font-semibold text-center mb-1">Power Off</h3>
                <p className="text-xs text-white/70 text-center mb-3">The system will power off automatically in 60 seconds.</p>
                <div className="flex flex-col gap-2">
                  <button onClick={() => useSystemStore.getState().sleep()} className="w-full py-2 rounded text-sm hover:bg-white/10 transition-colors" style={{ background: 'var(--bg-input)' }}>
                    Suspend
                  </button>
                  <button onClick={() => useSystemStore.getState().restart()} className="w-full py-2 rounded text-sm hover:bg-white/10 transition-colors" style={{ background: 'var(--bg-input)' }}>
                    Restart...
                  </button>
                  <button onClick={() => useSystemStore.getState().powerOff()} className="w-full py-2 rounded text-sm text-white hover:opacity-90 transition-opacity" style={{ background: 'var(--accent-dark-gray)' }}>
                    Shut Down
                  </button>
                </div>
                <button onClick={() => setShowPower(false)} className="w-full mt-2 text-xs text-white/50 hover:text-white/70 transition-colors">
                  Cancel
                </button>
              </motion.div>
            )}
          </AnimatePresence>
        </div>
      </div>
    </div>
  );
}
