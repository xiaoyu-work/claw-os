import { create } from 'zustand';
import { publicAsset } from '@/lib/publicAsset';

export interface WifiNetwork {
  ssid: string;
  strength: number;
  secured: boolean;
  connected: boolean;
}

export interface SettingsState {
  // Appearance
  theme: 'dark' | 'light';
  accentColor: string;
  wallpaper: string;
  wallpaperMode: 'fill' | 'fit' | 'stretch' | 'center' | 'tile';
  workspaceWallpapers: [string, string, string];

  // Sound
  outputVolume: number;
  inputVolume: number;
  outputDevice: string;
  inputDevice: string;
  muted: boolean;

  // Display
  brightness: number;
  resolution: string;
  scale: number;

  // Region
  language: string;
  region: string;
  timeFormat: '12h' | '24h';

  // Network (mock)
  wifiEnabled: boolean;
  wifiNetworks: WifiNetwork[];
  connectedWifi: string | null;
  ipAddress: string;

  // User
  username: string;
  hostname: string;

  // Actions
  setTheme: (theme: 'dark' | 'light') => void;
  setAccentColor: (color: string) => void;
  setWallpaper: (wallpaper: string) => void;
  setWallpaperMode: (mode: 'fill' | 'fit' | 'stretch' | 'center' | 'tile') => void;
  setWorkspaceWallpaper: (workspace: number, wallpaper: string) => void;
  setOutputVolume: (volume: number) => void;
  setInputVolume: (volume: number) => void;
  setOutputDevice: (device: string) => void;
  setInputDevice: (device: string) => void;
  setMuted: (muted: boolean) => void;
  setBrightness: (brightness: number) => void;
  setResolution: (resolution: string) => void;
  setScale: (scale: number) => void;
  setLanguage: (lang: string) => void;
  setRegion: (region: string) => void;
  setTimeFormat: (format: '12h' | '24h') => void;
  setWifiEnabled: (enabled: boolean) => void;
  connectWifi: (ssid: string) => void;
  disconnectWifi: () => void;
  setUsername: (name: string) => void;
  setHostname: (name: string) => void;
}

export const useSettingsStore = create<SettingsState>((set, get) => ({
  theme: 'dark',
  accentColor: '#7D8B96',
  wallpaper: publicAsset('wallpaper-concrete.jpg'),
  wallpaperMode: 'fill',
  workspaceWallpapers: [
    publicAsset('wallpaper-concrete.jpg'),
    publicAsset('wallpaper-frost.jpg'),
    publicAsset('wallpaper-marble.jpg'),
  ],

  outputVolume: 75,
  inputVolume: 50,
  outputDevice: 'Built-in Audio',
  inputDevice: 'Built-in Microphone',
  muted: false,

  brightness: 85,
  resolution: '1920 x 1080',
  scale: 100,

  language: 'en-US',
  region: 'United States',
  timeFormat: '24h',

  wifiEnabled: true,
  wifiNetworks: [
    { ssid: 'WebOS-Home-5G', strength: 90, secured: true, connected: true },
    { ssid: 'WebOS-Home-2.4G', strength: 85, secured: true, connected: false },
    { ssid: 'CoffeeShop_Free', strength: 60, secured: false, connected: false },
    { ssid: 'Neighbor_WiFi', strength: 40, secured: true, connected: false },
    { ssid: 'Starbucks_Guest', strength: 55, secured: true, connected: false },
    { ssid: 'Library_Public', strength: 70, secured: false, connected: false },
  ],
  connectedWifi: 'WebOS-Home-5G',
  ipAddress: '192.168.1.42',

  username: 'user',
  hostname: 'webos-desktop',

  setTheme: (theme) => set({ theme }),
  setAccentColor: (accentColor) => set({ accentColor }),
  setWallpaper: (wallpaper) => set({ wallpaper }),
  setWallpaperMode: (wallpaperMode) => set({ wallpaperMode }),
  setWorkspaceWallpaper: (workspace, wallpaper) =>
    set((state) => {
      const newWallpapers = [...state.workspaceWallpapers] as [string, string, string];
      newWallpapers[workspace - 1] = wallpaper;
      return { workspaceWallpapers: newWallpapers, wallpaper: newWallpapers[state.workspaceWallpapers.indexOf(state.wallpaper) === -1 ? 0 : 0] };
    }),
  setOutputVolume: (outputVolume) => set({ outputVolume }),
  setInputVolume: (inputVolume) => set({ inputVolume }),
  setOutputDevice: (outputDevice) => set({ outputDevice }),
  setInputDevice: (inputDevice) => set({ inputDevice }),
  setMuted: (muted) => set({ muted }),
  setBrightness: (brightness) => set({ brightness }),
  setResolution: (resolution) => set({ resolution }),
  setScale: (scale) => set({ scale }),
  setLanguage: (language) => set({ language }),
  setRegion: (region) => set({ region }),
  setTimeFormat: (timeFormat) => set({ timeFormat }),
  setWifiEnabled: (wifiEnabled) => set({ wifiEnabled, connectedWifi: wifiEnabled ? get().connectedWifi : null }),
  connectWifi: (ssid) =>
    set((state) => ({
      connectedWifi: ssid,
      wifiNetworks: state.wifiNetworks.map((w) => ({
        ...w,
        connected: w.ssid === ssid,
      })),
    })),
  disconnectWifi: () =>
    set((state) => ({
      connectedWifi: null,
      wifiNetworks: state.wifiNetworks.map((w) => ({ ...w, connected: false })),
    })),
  setUsername: (username) => set({ username }),
  setHostname: (hostname) => set({ hostname }),
}));
