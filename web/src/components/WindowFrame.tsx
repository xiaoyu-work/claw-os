import { useRef, type MouseEvent, type ReactNode } from 'react';
import { motion } from 'framer-motion';
import * as Icons from 'lucide-react';
import { useWindowStore } from '@/stores/useWindowStore';
import { useAppRegistryStore } from '@/stores/useAppRegistryStore';

interface WindowFrameProps {
  windowId: string;
  children: ReactNode;
}

export default function WindowFrame({ windowId, children }: WindowFrameProps) {
  const win = useWindowStore((s) => s.windows.find((w) => w.id === windowId));
  const closeWindow = useWindowStore((s) => s.closeWindow);
  const minimizeWindow = useWindowStore((s) => s.minimizeWindow);
  const maximizeWindow = useWindowStore((s) => s.maximizeWindow);
  const focusWindow = useWindowStore((s) => s.focusWindow);
  const moveWindow = useWindowStore((s) => s.moveWindow);

  const dragRef = useRef({ isDragging: false, startX: 0, startY: 0, winX: 0, winY: 0 });
  const resizeRef = useRef<{ isResizing: boolean; direction: string; startX: number; startY: number; startW: number; startH: number; startWinX: number; startWinY: number }>({
    isResizing: false, direction: '', startX: 0, startY: 0, startW: 0, startH: 0, startWinX: 0, startWinY: 0,
  });
  const resizeRefPointer = useRef<HTMLDivElement>(null);

  if (!win || win.isMinimized) return null;

  const appDef = useAppRegistryStore.getState().getApp(win.appId);
  const IconComponent = appDef?.icon ? (Icons as unknown as Record<string, React.ComponentType<{ size?: number; className?: string }>>)[appDef.icon] : null;

  const handleMouseDown = () => {
    focusWindow(windowId);
  };

  const handleTitleMouseDown = (e: MouseEvent) => {
    if (win.isMaximized) return;
    dragRef.current = {
      isDragging: true,
      startX: e.clientX,
      startY: e.clientY,
      winX: win.x,
      winY: win.y,
    };
    const handleMove = (ev: globalThis.MouseEvent) => {
      if (!dragRef.current.isDragging) return;
      const dx = ev.clientX - dragRef.current.startX;
      const dy = ev.clientY - dragRef.current.startY;
      const nx = Math.max(-win.width + 80, Math.min(window.innerWidth - 40, dragRef.current.winX + dx));
      const ny = Math.max(0, Math.min(window.innerHeight - 48 - 40, dragRef.current.winY + dy));
      moveWindow(windowId, nx, ny);
    };
    const handleUp = () => {
      dragRef.current.isDragging = false;
      window.removeEventListener('mousemove', handleMove);
      window.removeEventListener('mouseup', handleUp);
    };
    window.addEventListener('mousemove', handleMove);
    window.addEventListener('mouseup', handleUp);
  };

  const handleResizeMouseDown = (e: MouseEvent, direction: string) => {
    e.stopPropagation();
    e.preventDefault();
    resizeRef.current = {
      isResizing: true,
      direction,
      startX: e.clientX,
      startY: e.clientY,
      startW: win.width,
      startH: win.height,
      startWinX: win.x,
      startWinY: win.y,
    };
    const handleMove = (ev: globalThis.MouseEvent) => {
      if (!resizeRef.current.isResizing) return;
      const r = resizeRef.current;
      let newW = r.startW;
      let newH = r.startH;
      let newX = r.startWinX;
      let newY = r.startWinY;

      if (r.direction.includes('e')) newW = Math.max(240, Math.min(window.innerWidth - r.startWinX, r.startW + (ev.clientX - r.startX)));
      if (r.direction.includes('w')) {
        const dx = r.startX - ev.clientX;
        newW = Math.max(240, r.startW + dx);
        if (newW > 240) newX = r.startWinX - dx;
      }
      if (r.direction.includes('s')) newH = Math.max(160, Math.min(window.innerHeight - 36 - 48 - r.startWinY, r.startH + (ev.clientY - r.startY)));
      if (r.direction.includes('n')) {
        const dy = r.startY - ev.clientY;
        newH = Math.max(160, r.startH + dy);
        if (newH > 160) newY = Math.max(36, r.startWinY - dy);
      }

      const store = useWindowStore.getState();
      store.resizeWindow(windowId, newW, newH);
      if (r.direction.includes('w') || r.direction.includes('n')) {
        store.moveWindow(windowId, newX, newY);
      }
    };
    const handleUp = () => {
      resizeRef.current.isResizing = false;
      window.removeEventListener('mousemove', handleMove);
      window.removeEventListener('mouseup', handleUp);
    };
    window.addEventListener('mousemove', handleMove);
    window.addEventListener('mouseup', handleUp);
  };

  const resizeCursors: Record<string, string> = {
    n: 'ns-resize', s: 'ns-resize', e: 'ew-resize', w: 'ew-resize',
    ne: 'nesw-resize', nw: 'nwse-resize', se: 'nwse-resize', sw: 'nesw-resize',
  };

  return (
    <motion.div
      initial={{ scale: 0.9, opacity: 0 }}
      animate={{ scale: 1, opacity: 1 }}
      exit={{ scale: 0.95, opacity: 0 }}
      transition={{ duration: 0.2, ease: [0.175, 0.885, 0.32, 1.275] as [number, number, number, number] }}
      style={{
        position: 'absolute',
        left: win.isMaximized ? 0 : win.x,
        top: win.isMaximized ? 36 : win.y,
        width: win.isMaximized ? '100%' : win.width,
        height: win.isMaximized ? `calc(100% - 84px)` : win.height,
        zIndex: win.zIndex,
      }}
      className={`pointer-events-auto flex flex-col ${win.isMaximized ? '' : 'rounded-lg'} overflow-hidden`}
      onMouseDown={handleMouseDown}
    >
      {/* Title bar */}
      <div
        className={`h-8 flex items-center justify-between px-2 select-none ${win.isMaximized ? 'rounded-none' : 'rounded-t-lg'}`}
        style={{ background: win.isFocused ? 'var(--bg-window)' : 'var(--bg-window-inactive)' }}
        onMouseDown={handleTitleMouseDown}
        onDoubleClick={() => maximizeWindow(windowId)}
      >
        <div className="flex items-center gap-2 overflow-hidden">
          {IconComponent && <IconComponent size={16} className="text-[var(--text-secondary)] shrink-0" />}
          <span className="text-sm font-semibold text-[var(--text-primary)] truncate" style={{ fontSize: 14, letterSpacing: '0.01em' }}>
            {win.title}
          </span>
        </div>
        <div className="flex items-center gap-1 shrink-0">
          <button
            onClick={(e) => { e.stopPropagation(); minimizeWindow(windowId); }}
            className="w-6 h-6 flex items-center justify-center rounded hover:bg-[#5A7A8A]/20 transition-colors"
            title="Minimize"
          >
            <Icons.Minus size={14} className="text-[var(--text-secondary)]" />
          </button>
          <button
            onClick={(e) => { e.stopPropagation(); maximizeWindow(windowId); }}
            className="w-6 h-6 flex items-center justify-center rounded hover:bg-[#22C55E]/20 transition-colors"
            title={win.isMaximized ? 'Restore' : 'Maximize'}
          >
            {win.isMaximized ? <Icons.Copy size={12} className="text-[var(--text-secondary)]" /> : <Icons.Square size={12} className="text-[var(--text-secondary)]" />}
          </button>
          <button
            onClick={(e) => { e.stopPropagation(); closeWindow(windowId); }}
            className="w-6 h-6 flex items-center justify-center rounded hover:bg-[#EF4444]/20 transition-colors"
            title="Close"
          >
            <Icons.X size={14} className="text-[var(--text-secondary)]" />
          </button>
        </div>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-hidden" style={{ background: 'var(--bg-workspace)' }}>
        {children}
      </div>

      {/* Resize handles */}
      {!win.isMaximized && (
        <>
          {['n', 's', 'e', 'w', 'ne', 'nw', 'se', 'sw'].map((dir) => (
            <div
              key={dir}
              ref={dir === 'se' ? resizeRefPointer : undefined}
              className="absolute"
              style={{
                cursor: resizeCursors[dir],
                ...(dir === 'n' ? { top: -4, left: 8, right: 8, height: 8 } :
                   dir === 's' ? { bottom: -4, left: 8, right: 8, height: 8 } :
                   dir === 'e' ? { right: -4, top: 8, bottom: 8, width: 8 } :
                   dir === 'w' ? { left: -4, top: 8, bottom: 8, width: 8 } :
                   dir === 'ne' ? { top: -4, right: -4, width: 12, height: 12 } :
                   dir === 'nw' ? { top: -4, left: -4, width: 12, height: 12 } :
                   dir === 'se' ? { bottom: -4, right: -4, width: 12, height: 12 } :
                   { bottom: -4, left: -4, width: 12, height: 12 }),
              }}
              onMouseDown={(e) => handleResizeMouseDown(e, dir)}
            />
          ))}
        </>
      )}
    </motion.div>
  );
}
