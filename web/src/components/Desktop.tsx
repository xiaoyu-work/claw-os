import { useState, useCallback, useEffect, type ComponentType } from 'react';
import { Trash2, LayoutGrid } from 'lucide-react';
import { useSystemStore } from '@/stores/useSystemStore';
import { useWindowStore } from '@/stores/useWindowStore';
import { useAppRegistryStore } from '@/stores/useAppRegistryStore';
import { useFileSystemStore } from '@/stores/useFileSystemStore';
import { useSettingsStore } from '@/stores/useSettingsStore';
import { publicAsset } from '@/lib/publicAsset';
import AppIcon from '@/components/AppIcon';

interface DesktopIcon {
  id: string;
  label: string;
  appId: string;
  icon?: ComponentType<{ size?: number; className?: string }>;
}

const defaultIcons: DesktopIcon[] = [
  { id: 'd-activities', label: 'Activities', appId: '__activities__', icon: LayoutGrid },
  { id: 'd-agent', label: 'Claw OS Agent', appId: 'agent' },
  { id: 'd-browser', label: 'Claw OS Website', appId: 'browser' },
  { id: 'd-files', label: 'Files', appId: 'filemanager' },
  { id: 'd-editor', label: 'Text Editor', appId: 'texteditor' },
  { id: 'd-store', label: 'App Store', appId: 'store' },
  { id: 'd-player', label: 'Media Player', appId: 'player' },
  { id: 'd-screenshot', label: 'Screenshot', appId: 'screenshot' },
  { id: 'd-settings', label: 'Settings', appId: 'settings' },
  { id: 'd-trash', label: 'Trash', appId: 'filemanager', icon: Trash2 },
];

interface DesktopProps {
  onOpenAppMenu?: () => void;
}

export default function Desktop({ onOpenAppMenu }: DesktopProps) {
  const wallpaper = useSettingsStore((s) => s.wallpaper);
  const workspaceWallpapers = useSettingsStore((s) => s.workspaceWallpapers);
  const activeWorkspace = useSystemStore((s) => s.activeWorkspace);
  const openWindow = useWindowStore((s) => s.openWindow);
  const getApp = useAppRegistryStore((s) => s.getApp);
  const createDirectory = useFileSystemStore((s) => s.createDirectory);
  const createFile = useFileSystemStore((s) => s.createFile);
  const setWallpaper = useSettingsStore((s) => s.setWallpaper);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);
  const [icons] = useState<DesktopIcon[]>(defaultIcons);
  const [selectedIcon, setSelectedIcon] = useState<string | null>(null);

  // Use workspace-specific wallpaper
  const currentWallpaper = workspaceWallpapers[activeWorkspace - 1] || wallpaper;

  const handleOpenApp = useCallback((appId: string, customTitle?: string, preAction?: () => void) => {
    if (preAction) preAction();
    const app = getApp(appId);
    if (!app) return;
    openWindow(appId, customTitle || app.name, { width: app.defaultWidth, height: app.defaultHeight });
  }, [getApp, openWindow]);

  const handleContextMenu = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    setContextMenu({ x: e.clientX, y: e.clientY });
    setSelectedIcon(null);
  }, []);

  useEffect(() => {
    if (!contextMenu) return;
    const handler = () => setContextMenu(null);
    window.addEventListener('click', handler, { once: true });
    return () => window.removeEventListener('click', handler);
  }, [contextMenu]);

  return (
    <div
      className="fixed inset-0 z-[1]"
      style={{
        backgroundImage: `url(${currentWallpaper})`,
        backgroundSize: 'cover',
        backgroundPosition: 'center',
        top: 36,
        bottom: 48,
      }}
      onContextMenu={handleContextMenu}
      onClick={() => { setSelectedIcon(null); }}
    >
      {/* Dark overlay */}
      <div className="absolute inset-0" style={{ background: 'rgba(15,15,35,0.15)' }} />

      {/* Desktop Icons */}
      <div className="relative p-6 flex flex-col flex-wrap gap-4 content-start h-full" style={{ gap: '8px' }}>
        {icons.map((icon) => {
          const IconComp = icon.icon;
          const app = getApp(icon.appId);
          return (
            <button
              key={icon.id}
              data-guide-target={icon.id === 'd-agent' ? 'agent-desktop-icon' : undefined}
              className={`flex flex-col items-center w-20 py-2 rounded-lg transition-all group ${
                selectedIcon === icon.id ? 'bg-[rgba(125,139,150,0.2)]' : 'hover:bg-[rgba(255,255,255,0.08)] hover:-translate-y-0.5'
              }`}
              style={{ textShadow: '0 1px 3px rgba(0,0,0,0.3)' }}
              onClick={(e) => { e.stopPropagation(); setSelectedIcon(icon.id); }}
              onDoubleClick={() => {
                if (icon.label === 'Trash') {
                  useFileSystemStore.getState().setCurrentDirectory('fs-trash');
                  handleOpenApp(icon.appId, 'Trash');
                } else if (icon.label === 'Activities') {
                  onOpenAppMenu?.();
                } else if (icon.id === 'd-browser') {
                  handleOpenApp(icon.appId, 'Claw OS — Web Browser');
                } else {
                  handleOpenApp(icon.appId);
                }
              }}
            >
              {IconComp ? (
                <IconComp size={48} className="mb-1 text-[var(--text-primary)]" />
              ) : app ? (
                <AppIcon icon={app.icon} label={app.name} size={48} className="mb-1" />
              ) : null}
              <span className="text-[11px] text-[var(--text-primary)] text-center leading-tight max-w-full break-words">
                {icon.label}
              </span>
            </button>
          );
        })}
      </div>

      {/* Context Menu */}
      {contextMenu && (
        <div
          className="fixed rounded-lg py-1 z-[60] min-w-[180px]"
          style={{
            left: contextMenu.x,
            top: contextMenu.y,
            background: 'var(--bg-panel)',
            boxShadow: 'var(--shadow-md)',
            border: '1px solid rgba(0,0,0,0.10)',
          }}
        >
          <div className="px-3 py-1 text-xs font-bold uppercase tracking-wider text-[var(--text-muted)]">Create New</div>
          <button onClick={() => { createDirectory('New Folder', 'fs-user-desktop'); setContextMenu(null); }}
            className="w-full text-left px-3 py-1.5 text-sm text-[var(--text-primary)] hover:bg-[var(--bg-hover)] transition-colors">
            Folder
          </button>
          <button onClick={() => { createFile('New Document.txt', 'fs-user-desktop'); setContextMenu(null); }}
            className="w-full text-left px-3 py-1.5 text-sm text-[var(--text-primary)] hover:bg-[var(--bg-hover)] transition-colors">
            Text Document
          </button>
          <div className="my-1 h-px" style={{ background: 'rgba(0,0,0,0.06)' }} />
          <button onClick={() => { setWallpaper(publicAsset('wallpaper-concrete.jpg')); setContextMenu(null); }}
            className="w-full text-left px-3 py-1.5 text-sm text-[var(--text-primary)] hover:bg-[var(--bg-hover)] transition-colors">
            Change Background...
          </button>
          <button onClick={() => { handleOpenApp('settings'); setContextMenu(null); }}
            className="w-full text-left px-3 py-1.5 text-sm text-[var(--text-primary)] hover:bg-[var(--bg-hover)] transition-colors">
            Settings
          </button>
        </div>
      )}
    </div>
  );
}
