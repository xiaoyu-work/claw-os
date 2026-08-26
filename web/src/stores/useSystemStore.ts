import { create } from 'zustand';

export interface Notification {
  id: string;
  title: string;
  message: string;
  type: 'info' | 'success' | 'warning' | 'error';
  read: boolean;
  timestamp: Date;
}

export interface SystemState {
  currentTime: Date;
  workspaces: number[];
  activeWorkspace: number;
  notifications: Notification[];
  setCurrentTime: (time: Date) => void;
  setActiveWorkspace: (workspace: number) => void;
  addNotification: (notification: Omit<Notification, 'id' | 'timestamp' | 'read'>) => void;
  removeNotification: (id: string) => void;
  markNotificationRead: (id: string) => void;
  clearNotifications: () => void;
}

export const useSystemStore = create<SystemState>((set) => ({
  currentTime: new Date(),
  workspaces: [1, 2, 3],
  activeWorkspace: 1,
  notifications: [
    {
      id: 'welcome-1',
      title: 'Welcome to Claw OS',
      message: 'The Claw OS demo desktop is ready.',
      type: 'info',
      read: false,
      timestamp: new Date(),
    },
  ],

  setCurrentTime: (time) => set({ currentTime: time }),

  setActiveWorkspace: (workspace) => set({ activeWorkspace: workspace }),

  addNotification: (notification) =>
    set((state) => ({
      notifications: [
        {
          ...notification,
          id: `notif-${Date.now()}-${Math.random().toString(36).substring(2, 9)}`,
          timestamp: new Date(),
          read: false,
        },
        ...state.notifications,
      ],
    })),

  removeNotification: (id) =>
    set((state) => ({
      notifications: state.notifications.filter((notification) => notification.id !== id),
    })),

  markNotificationRead: (id) =>
    set((state) => ({
      notifications: state.notifications.map((notification) =>
        notification.id === id ? { ...notification, read: true } : notification
      ),
    })),

  clearNotifications: () => set({ notifications: [] }),
}));
