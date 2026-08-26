import { useState, useEffect } from 'react';
import { useSystemStore } from '@/stores/useSystemStore';
import { useWindowStore } from '@/stores/useWindowStore';
import { useSettingsStore } from '@/stores/useSettingsStore';
import WindowFrame from '@/components/WindowFrame';
import TopPanel from '@/components/TopPanel';
import BottomTaskbar from '@/components/BottomTaskbar';
import ApplicationMenu from '@/components/ApplicationMenu';
import Desktop from '@/components/Desktop';
import GuideOverlay from '@/components/GuideOverlay';
import { renderApp } from '@/components/AppRegistry';

export default function App() {
  const windows = useWindowStore((s) => s.windows);
  const activeWorkspace = useSystemStore((s) => s.activeWorkspace);
  const [appMenuOpen, setAppMenuOpen] = useState(false);

  // Clock tick
  useEffect(() => {
    const interval = setInterval(() => {
      useSystemStore.getState().setCurrentTime(new Date());
    }, 1000);
    return () => clearInterval(interval);
  }, []);

  // Keyboard shortcut: Super key opens app menu
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Meta') { e.preventDefault(); setAppMenuOpen((p) => !p); }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, []);

  // Dynamic CSS variables from settings
  const accentColor = useSettingsStore((s) => s.accentColor);
  const theme = useSettingsStore((s) => s.theme);

  useEffect(() => {
    document.documentElement.style.setProperty('--accent-silver', accentColor);
    document.documentElement.style.setProperty('--text-accent', accentColor);
    document.documentElement.style.setProperty('--scrollbar-accent', accentColor);
    document.documentElement.style.setProperty('--border-active', accentColor);
  }, [accentColor]);

  useEffect(() => {
    if (theme === 'light') {
      document.documentElement.classList.add('light-theme');
      document.documentElement.classList.remove('dark-theme');
    } else {
      document.documentElement.classList.add('dark-theme');
      document.documentElement.classList.remove('light-theme');
    }
  }, [theme]);

  return (
    <div className="fixed inset-0 overflow-hidden" style={{ background: 'var(--bg-base)' }}>
      {/* Desktop */}
      <Desktop onOpenAppMenu={() => setAppMenuOpen(true)} />

      {/* Window Layer */}
      <div className="fixed inset-0 z-[10] overflow-hidden pointer-events-none">
        {windows
          .filter((w) => w.workspace === activeWorkspace && !w.isMinimized)
          .map((win) => (
            <div key={win.id} className="pointer-events-none" style={{ position: 'absolute', inset: 0 }}>
              <WindowFrame windowId={win.id}>
                {renderApp(win.appId, win.id)}
              </WindowFrame>
            </div>
          ))}
      </div>

      {/* Top Panel */}
      <TopPanel onOpenAppMenu={() => setAppMenuOpen(!appMenuOpen)} />

      {/* Bottom Taskbar */}
      <BottomTaskbar />

      {/* Application Menu */}
      <ApplicationMenu isOpen={appMenuOpen} onClose={() => setAppMenuOpen(false)} />

      {/* Forced first-run demo path */}
      <GuideOverlay />
    </div>
  );
}
