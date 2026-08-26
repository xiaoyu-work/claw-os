import { useCallback } from 'react';
import { useAppRegistryStore } from '@/stores/useAppRegistryStore';
import { useWindowStore } from '@/stores/useWindowStore';
import { Crown, Bomb, LayoutGrid, Snail, Club, Grid3X3, XCircle, Brain, Grid2X2, CircleDot, Gamepad2 } from 'lucide-react';

import type { LucideProps } from 'lucide-react';

const iconMap: Record<string, React.ComponentType<LucideProps>> = {
  Crown, Bomb, LayoutGrid, Snail, Club, Grid3X3, XCircle, Brain, Grid2X2, CircleDot,
};

export default function GameLauncher({ windowId: _windowId }: { windowId: string }) {
  const getAppsByCategory = useAppRegistryStore((s) => s.getAppsByCategory);
  const openWindow = useWindowStore((s) => s.openWindow);
  const getApp = useAppRegistryStore((s) => s.getApp);

  const games = getAppsByCategory('Games');

  const handleOpenGame = useCallback((appId: string) => {
    const app = getApp(appId);
    if (!app) return;
    openWindow(appId, app.name, {
      width: app.defaultWidth,
      height: app.defaultHeight,
    });
  }, [getApp, openWindow]);

  return (
    <div className="w-full h-full flex flex-col p-6" style={{ background: '#1A1A1A' }}>
      {/* Header */}
      <div className="flex items-center gap-3 mb-6">
        <Gamepad2 size={28} className="text-white" />
        <h1 className="text-xl font-bold text-white">Games</h1>
        <span className="text-sm text-white/50 ml-2">{games.length} games available</span>
      </div>

      {/* Game Grid */}
      <div className="grid grid-cols-3 gap-4 overflow-y-auto">
        {games.map((game) => {
          const IconComp = iconMap[game.icon] || Gamepad2;
          return (
            <button
              key={game.id}
              onClick={() => handleOpenGame(game.id)}
              className="flex flex-col items-center gap-3 p-5 rounded-xl transition-all hover:scale-105 hover:bg-white/10"
              style={{ background: 'rgba(255,255,255,0.05)', border: '1px solid rgba(255,255,255,0.08)' }}
            >
              <div className="w-14 h-14 flex items-center justify-center rounded-lg" style={{ background: 'rgba(125,139,150,0.2)' }}>
                <IconComp size={28} className="text-white" />
              </div>
              <div className="text-center">
                <div className="text-sm font-medium text-white">{game.name}</div>
                <div className="text-xs text-white/50 mt-1">{game.description}</div>
              </div>
            </button>
          );
        })}
      </div>
    </div>
  );
}
