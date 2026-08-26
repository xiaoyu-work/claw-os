import { useSettingsStore } from '@/stores/useSettingsStore';

export default function DisplayTab() {
  const { brightness, setBrightness, resolution, setResolution, scale, setScale } = useSettingsStore();

  return (
    <div className="max-w-xl space-y-6">
      <h2 className="text-lg font-semibold mb-4">Display</h2>

      <div>
        <label className="text-sm font-medium text-[var(--text-secondary)] mb-2 block">Brightness: {brightness}%</label>
        <input
          type="range" min={10} max={100} value={brightness}
          onChange={(e) => setBrightness(Number(e.target.value))}
          className="w-full accent-[var(--accent-dark-gray)]"
        />
      </div>

      <div>
        <label className="text-sm font-medium text-[var(--text-secondary)] mb-2 block">Resolution</label>
        <select
          value={resolution}
          onChange={(e) => setResolution(e.target.value)}
          className="w-full h-10 px-3 rounded text-sm outline-none"
          style={{ background: 'var(--bg-input)', color: 'var(--text-primary)', border: '1px solid rgba(0,0,0,0.06)' }}
        >
          <option>3840 x 2160</option>
          <option>2560 x 1440</option>
          <option>1920 x 1080</option>
          <option>1680 x 1050</option>
          <option>1440 x 900</option>
          <option>1280 x 720</option>
        </select>
      </div>

      <div>
        <label className="text-sm font-medium text-[var(--text-secondary)] mb-2 block">Scale: {scale}%</label>
        <input
          type="range" min={50} max={200} step={25} value={scale}
          onChange={(e) => setScale(Number(e.target.value))}
          className="w-full accent-[var(--accent-dark-gray)]"
        />
        <div className="flex justify-between text-xs text-[var(--text-muted)] mt-1">
          <span>50%</span>
          <span>100%</span>
          <span>150%</span>
          <span>200%</span>
        </div>
      </div>
    </div>
  );
}
