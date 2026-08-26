import { useSettingsStore } from '@/stores/useSettingsStore';
import { publicAsset } from '@/lib/publicAsset';

export default function AppearanceTab() {
  const wallpaper = useSettingsStore((s) => s.wallpaper);
  const setWallpaper = useSettingsStore((s) => s.setWallpaper);
  const accentColor = useSettingsStore((s) => s.accentColor);
  const setAccentColor = useSettingsStore((s) => s.setAccentColor);
  const theme = useSettingsStore((s) => s.theme);
  const setTheme = useSettingsStore((s) => s.setTheme);

  const wallpapers = [
    { path: publicAsset('wallpaper-concrete.jpg'), name: 'White Concrete' },
    { path: publicAsset('wallpaper-frost.jpg'), name: 'Frost Glass' },
    { path: publicAsset('wallpaper-marble.jpg'), name: 'Light Marble' },
  ];

  const accentColors = [
    '#7D8B96', '#5A6670', '#B89A60', '#6A7A8A', '#8A9AA6',
    '#8A8A8A', '#A0AAB0', '#7A8A96', '#8A96A2', '#5A6A72',
  ];

  return (
    <div className="max-w-xl space-y-6">
      <h2 className="text-lg font-semibold mb-4">Appearance</h2>

      {/* Wallpaper */}
      <div>
        <label className="text-sm font-medium text-[var(--text-secondary)] mb-2 block">Wallpaper</label>
        <div className="grid grid-cols-3 gap-3">
          {wallpapers.map((wp) => (
            <button
              key={wp.path}
              onClick={() => setWallpaper(wp.path)}
              className={`relative rounded-lg overflow-hidden aspect-video ${wallpaper === wp.path ? 'ring-2 ring-[var(--accent-silver)]' : ''}`}
            >
              <img src={wp.path} alt={wp.name} className="w-full h-full object-cover" />
              <span className="absolute bottom-0 inset-x-0 text-xs text-white text-center py-1" style={{ background: 'rgba(0,0,0,0.6)' }}>{wp.name}</span>
            </button>
          ))}
        </div>
      </div>

      {/* Theme */}
      <div>
        <label className="text-sm font-medium text-[var(--text-secondary)] mb-2 block">Theme</label>
        <div className="flex gap-2">
          <button
            onClick={() => setTheme('dark')}
            className={`px-4 py-2 rounded-lg text-sm transition-colors flex items-center gap-2 ${theme === 'dark' ? 'text-white' : 'text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]'}`}
            style={theme === 'dark' ? { background: accentColor } : { background: 'var(--bg-input)' }}
          >
            {theme === 'dark' && <span>✓</span>} Dark
          </button>
          <button
            onClick={() => setTheme('light')}
            className={`px-4 py-2 rounded-lg text-sm transition-colors flex items-center gap-2 ${theme === 'light' ? 'text-white' : 'text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]'}`}
            style={theme === 'light' ? { background: accentColor } : { background: 'var(--bg-input)' }}
          >
            {theme === 'light' && <span>✓</span>} Light
          </button>
        </div>
      </div>

      {/* Accent Color */}
      <div>
        <label className="text-sm font-medium text-[var(--text-secondary)] mb-2 block">Accent Color</label>
        <div className="flex flex-wrap gap-2">
          {accentColors.map((color) => (
            <button
              key={color}
              onClick={() => setAccentColor(color)}
              className={`w-8 h-8 rounded-full transition-transform hover:scale-110 flex items-center justify-center ${accentColor === color ? 'ring-2 ring-white ring-offset-2 ring-offset-[var(--bg-workspace)]' : ''}`}
              style={{ background: color }}
            >
              {accentColor === color && <span className="text-white text-xs font-bold">✓</span>}
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
