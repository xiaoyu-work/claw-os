import { Volume2, Mic, Headphones } from 'lucide-react';
import { useSettingsStore } from '@/stores/useSettingsStore';

export default function SoundTab() {
  const { outputVolume, setOutputVolume, inputVolume, setInputVolume, outputDevice, setOutputDevice, inputDevice, setInputDevice, muted, setMuted } = useSettingsStore();

  return (
    <div className="max-w-xl space-y-6">
      <h2 className="text-lg font-semibold mb-4">Sound</h2>

      {/* Output */}
      <div className="p-4 rounded-lg space-y-3" style={{ background: 'var(--bg-window)' }}>
        <div className="flex items-center gap-2 mb-2">
          <Volume2 size={18} className="text-[var(--accent-silver)]" />
          <span className="font-medium">Output</span>
          <button onClick={() => setMuted(!muted)} className="ml-auto text-xs px-2 py-1 rounded" style={{ background: 'var(--bg-input)' }}>
            {muted ? 'Unmute' : 'Mute'}
          </button>
        </div>
        <div>
          <label className="text-xs text-[var(--text-muted)] mb-1 block">Volume: {muted ? 0 : outputVolume}%</label>
          <input
            type="range" min={0} max={100} value={muted ? 0 : outputVolume}
            onChange={(e) => { setOutputVolume(Number(e.target.value)); if (Number(e.target.value) > 0) setMuted(false); }}
            className="w-full accent-[var(--accent-dark-gray)]"
          />
        </div>
        <div>
          <label className="text-xs text-[var(--text-muted)] mb-1 block">Output Device</label>
          <select
            value={outputDevice}
            onChange={(e) => setOutputDevice(e.target.value)}
            className="w-full h-9 px-3 rounded text-sm outline-none"
            style={{ background: 'var(--bg-input)', color: 'var(--text-primary)', border: '1px solid rgba(0,0,0,0.06)' }}
          >
            <option>Built-in Audio</option>
            <option>HDMI Audio</option>
            <option>USB Audio</option>
            <option>Bluetooth Headphones</option>
          </select>
        </div>
      </div>

      {/* Input */}
      <div className="p-4 rounded-lg space-y-3" style={{ background: 'var(--bg-window)' }}>
        <div className="flex items-center gap-2 mb-2">
          <Mic size={18} className="text-[var(--accent-silver)]" />
          <span className="font-medium">Input</span>
        </div>
        <div>
          <label className="text-xs text-[var(--text-muted)] mb-1 block">Volume: {inputVolume}%</label>
          <input
            type="range" min={0} max={100} value={inputVolume}
            onChange={(e) => setInputVolume(Number(e.target.value))}
            className="w-full accent-[var(--accent-dark-gray)]"
          />
        </div>
        <div>
          <label className="text-xs text-[var(--text-muted)] mb-1 block">Input Device</label>
          <select
            value={inputDevice}
            onChange={(e) => setInputDevice(e.target.value)}
            className="w-full h-9 px-3 rounded text-sm outline-none"
            style={{ background: 'var(--bg-input)', color: 'var(--text-primary)', border: '1px solid rgba(0,0,0,0.06)' }}
          >
            <option>Built-in Microphone</option>
            <option>USB Microphone</option>
            <option>Bluetooth Headset</option>
          </select>
        </div>
      </div>
    </div>
  );
}
