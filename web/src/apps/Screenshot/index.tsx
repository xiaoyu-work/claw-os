import { useEffect, useRef, useState } from 'react';
import { Check, Clock3, Crosshair, Monitor, MousePointer2, PanelsTopLeft } from 'lucide-react';

type CaptureMode = 'screen' | 'window' | 'area';

const modes: Array<{ id: CaptureMode; label: string; icon: typeof Monitor }> = [
  { id: 'screen', label: 'Screen', icon: Monitor },
  { id: 'window', label: 'Window', icon: PanelsTopLeft },
  { id: 'area', label: 'Area', icon: Crosshair },
];

export default function Screenshot() {
  const [mode, setMode] = useState<CaptureMode>('screen');
  const [delay, setDelay] = useState(0);
  const [includePointer, setIncludePointer] = useState(true);
  const [capturing, setCapturing] = useState(false);
  const [capturedAt, setCapturedAt] = useState<string | null>(null);
  const timer = useRef<number | null>(null);

  useEffect(() => () => {
    if (timer.current !== null) window.clearTimeout(timer.current);
  }, []);

  const capture = () => {
    if (capturing) return;
    setCapturing(true);
    setCapturedAt(null);
    timer.current = window.setTimeout(() => {
      setCapturedAt(new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' }));
      setCapturing(false);
      timer.current = null;
    }, delay * 1000 + 350);
  };

  return (
    <div className="flex h-full min-h-0 flex-col text-sm" style={{ background: 'var(--bg-workspace)' }}>
      <div className="grid flex-1 gap-5 overflow-y-auto p-5 md:grid-cols-[minmax(0,1fr)_240px]">
        <section className="relative min-h-64 overflow-hidden rounded-2xl border" style={{ background: '#1f2025', borderColor: 'rgba(0,0,0,0.08)' }}>
          <div className="absolute inset-x-0 top-0 flex h-8 items-center justify-between bg-black/45 px-3 text-[10px] text-white/55">
            <span>Claw OS Demo</span>
            <span>Screenshot preview</span>
          </div>
          <div className="absolute inset-x-8 bottom-8 top-14 rounded-xl border border-white/10 bg-gradient-to-br from-slate-500/40 to-slate-900/70 p-4">
            <div className="grid h-full grid-cols-3 gap-3">
              <div className="rounded-lg bg-white/10" />
              <div className="col-span-2 rounded-lg bg-white/[0.07]" />
              <div className="col-span-3 rounded-lg bg-[#005CFE]/25" />
            </div>
          </div>
          {mode === 'area' && <div className="absolute inset-x-20 bottom-20 top-24 border-2 border-dashed border-[#005CFE]" />}
          {mode === 'window' && <div className="absolute inset-x-12 bottom-12 top-16 rounded-xl border-2 border-[#005CFE]" />}
          {includePointer && <MousePointer2 size={22} className="absolute bottom-20 right-24 text-white drop-shadow" fill="#1f1f20" />}
          {capturedAt && (
            <div className="absolute bottom-3 left-1/2 flex -translate-x-1/2 items-center gap-2 rounded-full bg-black/70 px-3 py-1.5 text-xs text-white">
              <Check size={13} className="text-[#005CFE]" />
              Demo capture saved at {capturedAt}
            </div>
          )}
        </section>

        <aside className="space-y-5 rounded-2xl border p-4" style={{ background: 'var(--bg-window)', borderColor: 'rgba(0,0,0,0.06)' }}>
          <div>
            <h3 className="mb-2 text-xs font-semibold uppercase tracking-wider text-[var(--text-muted)]">Capture</h3>
            <div className="grid grid-cols-3 gap-2 md:grid-cols-1">
              {modes.map((item) => {
                const Icon = item.icon;
                return (
                  <button
                    key={item.id}
                    onClick={() => setMode(item.id)}
                    className="flex items-center justify-center gap-2 rounded-lg px-3 py-2 text-xs transition-colors md:justify-start"
                    style={{
                      background: mode === item.id ? 'var(--bg-active)' : 'var(--bg-input)',
                      color: mode === item.id ? 'var(--text-primary)' : 'var(--text-secondary)',
                    }}
                  >
                    <Icon size={15} />
                    {item.label}
                  </button>
                );
              })}
            </div>
          </div>

          <div>
            <h3 className="mb-2 flex items-center gap-1.5 text-xs font-semibold uppercase tracking-wider text-[var(--text-muted)]">
              <Clock3 size={13} />
              Delay
            </h3>
            <div className="flex gap-2">
              {[0, 3, 5].map((seconds) => (
                <button
                  key={seconds}
                  onClick={() => setDelay(seconds)}
                  className="flex-1 rounded-lg py-2 text-xs"
                  style={{
                    background: delay === seconds ? 'var(--accent-silver)' : 'var(--bg-input)',
                    color: delay === seconds ? '#fff' : 'var(--text-secondary)',
                  }}
                >
                  {seconds}s
                </button>
              ))}
            </div>
          </div>

          <label className="flex cursor-pointer items-center justify-between rounded-lg p-3" style={{ background: 'var(--bg-input)' }}>
            <span className="flex items-center gap-2 text-xs text-[var(--text-secondary)]">
              <MousePointer2 size={14} />
              Include pointer
            </span>
            <input
              type="checkbox"
              checked={includePointer}
              onChange={(event) => setIncludePointer(event.target.checked)}
              className="accent-[#005CFE]"
            />
          </label>

          <button
            onClick={capture}
            disabled={capturing}
            className="w-full rounded-xl bg-[#005CFE] py-3 text-sm font-medium text-white transition-opacity hover:opacity-90 disabled:opacity-50"
          >
            {capturing ? `Capturing${delay ? ` in ${delay}s` : ''}…` : 'Capture demo'}
          </button>
        </aside>
      </div>
    </div>
  );
}
