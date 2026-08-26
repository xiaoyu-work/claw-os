import { create } from 'zustand';
import { useSystemStore } from './useSystemStore';

export interface WindowInstance {
  id: string;
  appId: string;
  title: string;
  x: number;
  y: number;
  width: number;
  height: number;
  isMinimized: boolean;
  isMaximized: boolean;
  isFocused: boolean;
  zIndex: number;
  workspace: number;
  prevX?: number;
  prevY?: number;
  prevWidth?: number;
  prevHeight?: number;
}

export interface WindowStore {
  windows: WindowInstance[];
  nextZIndex: number;
  openWindow: (appId: string, title: string, config?: Partial<Omit<WindowInstance, 'id' | 'appId' | 'title'>>) => string;
  closeWindow: (id: string) => void;
  minimizeWindow: (id: string) => void;
  maximizeWindow: (id: string) => void;
  focusWindow: (id: string) => void;
  moveWindow: (id: string, x: number, y: number) => void;
  resizeWindow: (id: string, width: number, height: number) => void;
  restoreWindow: (id: string) => void;
  getWindowsByWorkspace: (workspace: number) => WindowInstance[];
  getWindowById: (id: string) => WindowInstance | undefined;
  setWindowWorkspace: (id: string, workspace: number) => void;
  minimizeAll: () => void;
  unminimizeAll: () => void;
}

export const useWindowStore = create<WindowStore>((set, get) => ({
  windows: [],
  nextZIndex: 10,

  openWindow: (appId, title, config = {}) => {
    const state = get();
    const windowId = `win-${Date.now()}-${Math.random().toString(36).substring(2, 9)}`;
    const activeWorkspace = useSystemStore.getState().activeWorkspace;
    // Check for singleton windows
    const existingSingleton = state.windows.find(
      (w) => w.appId === appId && !w.isMinimized && w.workspace === activeWorkspace
    );
    if (existingSingleton) {
      get().focusWindow(existingSingleton.id);
      return existingSingleton.id;
    }

    const defaultWidth = config.width || 800;
    const defaultHeight = config.height || 600;
    const vw = typeof window !== 'undefined' ? window.innerWidth : 1200;
    const vh = typeof window !== 'undefined' ? window.innerHeight : 800;
    const x = config.x ?? Math.max(40, (vw - defaultWidth) / 2 + (state.windows.length * 20) % 120);
    const y = config.y ?? Math.max(40, (vh - defaultHeight) / 2 + (state.windows.length * 20) % 120);

    const newWindow: WindowInstance = {
      id: windowId,
      appId,
      title,
      x,
      y,
      width: Math.min(defaultWidth, vw - 80),
      height: Math.min(defaultHeight, vh - 100),
      isMinimized: false,
      isMaximized: false,
      isFocused: true,
      zIndex: state.nextZIndex,
      workspace: config.workspace ?? activeWorkspace,
      ...config,
    };

    set({
      windows: [
        ...state.windows.map((w) => ({ ...w, isFocused: false })),
        newWindow,
      ],
      nextZIndex: state.nextZIndex + 1,
    });

    return windowId;
  },

  closeWindow: (id) =>
    set((state) => ({
      windows: state.windows.filter((w) => w.id !== id),
    })),

  minimizeWindow: (id) =>
    set((state) => ({
      windows: state.windows.map((w) =>
        w.id === id ? { ...w, isMinimized: true, isFocused: false } : w
      ),
    })),

  maximizeWindow: (id) =>
    set((state) => ({
      windows: state.windows.map((w) => {
        if (w.id !== id) return w;
        if (w.isMaximized) {
          return {
            ...w,
            isMaximized: false,
            x: w.prevX ?? w.x,
            y: w.prevY ?? w.y,
            width: w.prevWidth ?? w.width,
            height: w.prevHeight ?? w.height,
          };
        }
        return {
          ...w,
          isMaximized: true,
          prevX: w.x,
          prevY: w.y,
          prevWidth: w.width,
          prevHeight: w.height,
          x: 0,
          y: 36,
          width: window.innerWidth,
          height: window.innerHeight - 36 - 48,
        };
      }),
    })),

  focusWindow: (id) => {
    const state = get();
    const target = state.windows.find((w) => w.id === id);
    if (!target || target.isMinimized) return;

    // Switch to the window's workspace
    if (target.workspace !== useSystemStore.getState().activeWorkspace) {
      useSystemStore.getState().setActiveWorkspace(target.workspace);
    }

    const newZIndex = state.nextZIndex;
    set({
      windows: state.windows.map((w) =>
        w.id === id
          ? { ...w, isFocused: true, zIndex: newZIndex, isMinimized: false }
          : { ...w, isFocused: w.isFocused ? false : w.isFocused }
      ),
      nextZIndex: state.nextZIndex + 1,
    });
  },

  moveWindow: (id, x, y) =>
    set((state) => ({
      windows: state.windows.map((w) =>
        w.id === id ? { ...w, x, y } : w
      ),
    })),

  resizeWindow: (id, width, height) =>
    set((state) => ({
      windows: state.windows.map((w) =>
        w.id === id ? { ...w, width, height } : w
      ),
    })),

  restoreWindow: (id) =>
    set((state) => ({
      windows: state.windows.map((w) =>
        w.id === id ? { ...w, isMinimized: false } : w
      ),
    })),

  getWindowsByWorkspace: (workspace) => {
    return get().windows.filter((w) => w.workspace === workspace);
  },

  getWindowById: (id) => {
    return get().windows.find((w) => w.id === id);
  },

  setWindowWorkspace: (id, workspace) =>
    set((state) => ({
      windows: state.windows.map((w) =>
        w.id === id ? { ...w, workspace } : w
      ),
    })),

  minimizeAll: () =>
    set((state) => ({
      windows: state.windows.map((w) => ({ ...w, isMinimized: true, isFocused: false })),
    })),

  unminimizeAll: () =>
    set((state) => ({
      windows: state.windows.map((w) => ({ ...w, isMinimized: false })),
    })),
}));
