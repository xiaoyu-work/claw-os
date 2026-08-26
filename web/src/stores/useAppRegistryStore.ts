import { create } from 'zustand';
import { publicAsset } from '@/lib/publicAsset';

export interface AppDefinition {
  id: string;
  name: string;
  description: string;
  category: string;
  icon: string;
  component: string;
  defaultWidth: number;
  defaultHeight: number;
  minWidth: number;
  minHeight: number;
  singleton: boolean;
}

export interface AppRegistryStore {
  apps: Record<string, AppDefinition>;
  registerApp: (app: AppDefinition) => void;
  getApp: (id: string) => AppDefinition | undefined;
  getAppsByCategory: (category: string) => AppDefinition[];
  getAllCategories: () => string[];
}

const appIcon = (name: string) => publicAsset(`app-icons/${name}.svg`);

const defaultApps: Record<string, AppDefinition> = {
  agent: {
    id: 'agent',
    name: 'Claw OS Agent',
    description: 'Ask system questions, inspect apps, and coordinate workflows',
    category: 'System',
    icon: appIcon('agent'),
    component: 'Agent',
    defaultWidth: 980,
    defaultHeight: 680,
    minWidth: 560,
    minHeight: 420,
    singleton: true,
  },
  filemanager: {
    id: 'filemanager',
    name: 'Files',
    description: 'Browse and manage files',
    category: 'Utilities',
    icon: appIcon('files'),
    component: 'FileManager',
    defaultWidth: 900,
    defaultHeight: 600,
    minWidth: 500,
    minHeight: 300,
    singleton: false,
  },
  texteditor: {
    id: 'texteditor',
    name: 'Text Editor',
    description: 'Create and edit text files',
    category: 'Utilities',
    icon: appIcon('edit'),
    component: 'TextEditor',
    defaultWidth: 800,
    defaultHeight: 600,
    minWidth: 400,
    minHeight: 300,
    singleton: false,
  },
  store: {
    id: 'store',
    name: 'App Store',
    description: 'Browse first-party Claw OS applications',
    category: 'System',
    icon: appIcon('store'),
    component: 'AppStore',
    defaultWidth: 940,
    defaultHeight: 640,
    minWidth: 520,
    minHeight: 400,
    singleton: true,
  },
  player: {
    id: 'player',
    name: 'Media Player',
    description: 'Play local audio and video',
    category: 'Multimedia',
    icon: appIcon('player'),
    component: 'MediaPlayer',
    defaultWidth: 850,
    defaultHeight: 600,
    minWidth: 500,
    minHeight: 380,
    singleton: true,
  },
  screenshot: {
    id: 'screenshot',
    name: 'Screenshot',
    description: 'Capture the screen, a window, or a selected area',
    category: 'Utilities',
    icon: appIcon('screenshot'),
    component: 'Screenshot',
    defaultWidth: 780,
    defaultHeight: 540,
    minWidth: 500,
    minHeight: 380,
    singleton: true,
  },
  settings: {
    id: 'settings',
    name: 'Settings',
    description: 'Configure the Claw OS demo desktop',
    category: 'System',
    icon: appIcon('settings'),
    component: 'Settings',
    defaultWidth: 800,
    defaultHeight: 550,
    minWidth: 600,
    minHeight: 400,
    singleton: true,
  },
  browser: {
    id: 'browser',
    name: 'Claw OS Website',
    description: 'Explore the Claw OS agent-native vision',
    category: 'Internet',
    icon: 'Globe',
    component: 'Browser',
    defaultWidth: 1100,
    defaultHeight: 700,
    minWidth: 600,
    minHeight: 400,
    singleton: false,
  },
};

export const useAppRegistryStore = create<AppRegistryStore>((set, get) => ({
  apps: defaultApps,

  registerApp: (app) =>
    set((state) => ({
      apps: { ...state.apps, [app.id]: app },
    })),

  getApp: (id) => get().apps[id],

  getAppsByCategory: (category) =>
    Object.values(get().apps).filter((app) => app.category === category),

  getAllCategories: () => {
    const categories = new Set(Object.values(get().apps).map((app) => app.category));
    return Array.from(categories).sort();
  },
}));
