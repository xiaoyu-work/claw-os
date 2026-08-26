import { useEffect, useState } from 'react';
import { Music2, Pause, Play, SkipBack, SkipForward, Volume2 } from 'lucide-react';

const tracks = [
  { title: 'Ambient Systems', artist: 'Claw OS Demo', duration: 214 },
  { title: 'Local First', artist: 'Claw OS Demo', duration: 188 },
  { title: 'Agent Workflow', artist: 'Claw OS Demo', duration: 242 },
];

function formatTime(seconds: number) {
  const minutes = Math.floor(seconds / 60);
  return `${minutes}:${String(seconds % 60).padStart(2, '0')}`;
}

export default function MediaPlayer() {
  const [selected, setSelected] = useState(0);
  const [playing, setPlaying] = useState(false);
  const [progress, setProgress] = useState(0);
  const [volume, setVolume] = useState(72);
  const track = tracks[selected];

  useEffect(() => {
    if (!playing) return;
    const timer = window.setInterval(() => {
      setProgress((current) => {
        if (current >= track.duration - 1) {
          setSelected((index) => (index + 1) % tracks.length);
          return 0;
        }
        return current + 1;
      });
    }, 1000);
    return () => window.clearInterval(timer);
  }, [playing, track.duration]);

  const chooseTrack = (index: number) => {
    setSelected(index);
    setProgress(0);
  };

  const previous = () => chooseTrack((selected - 1 + tracks.length) % tracks.length);
  const next = () => chooseTrack((selected + 1) % tracks.length);

  return (
    <div className="flex h-full min-h-0 flex-col text-sm" style={{ background: 'var(--bg-workspace)' }}>
      <div className="flex flex-1 flex-col gap-5 overflow-y-auto p-5 sm:flex-row">
        <section className="flex min-h-64 flex-1 flex-col items-center justify-center rounded-2xl border p-6" style={{ background: 'linear-gradient(145deg, #171719, #0a0a0b)', borderColor: 'rgba(255,255,255,0.08)' }}>
          <div className="relative mb-5 flex h-40 w-40 items-center justify-center overflow-hidden rounded-2xl bg-[#005CFE] shadow-2xl shadow-[#005CFE]/20">
            <div className="absolute inset-0 opacity-30" style={{ backgroundImage: 'radial-gradient(circle at 30% 30%, white 0, transparent 42%)' }} />
            <Music2 size={64} className="relative text-white" />
          </div>
          <h2 className="text-lg font-semibold text-white">{track.title}</h2>
          <p className="mt-1 text-xs text-white/45">{track.artist}</p>
        </section>

        <aside className="w-full shrink-0 rounded-2xl border p-3 sm:w-64" style={{ background: 'var(--bg-window)', borderColor: 'rgba(0,0,0,0.06)' }}>
          <h3 className="px-2 pb-2 text-xs font-semibold uppercase tracking-wider text-[var(--text-muted)]">Demo playlist</h3>
          <div className="space-y-1">
            {tracks.map((item, index) => (
              <button
                key={item.title}
                onClick={() => chooseTrack(index)}
                className="flex w-full items-center gap-3 rounded-xl p-2 text-left transition-colors"
                style={{ background: selected === index ? 'var(--bg-active)' : 'transparent' }}
              >
                <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg" style={{ background: 'var(--bg-input)' }}>
                  {selected === index && playing ? <Pause size={15} /> : <Play size={15} />}
                </div>
                <div className="min-w-0">
                  <div className="truncate text-xs font-medium text-[var(--text-primary)]">{item.title}</div>
                  <div className="text-[10px] text-[var(--text-muted)]">{formatTime(item.duration)}</div>
                </div>
              </button>
            ))}
          </div>
        </aside>
      </div>

      <footer className="shrink-0 border-t px-5 py-4" style={{ background: 'var(--bg-window)', borderColor: 'rgba(0,0,0,0.06)' }}>
        <div className="mb-3 flex items-center gap-3">
          <span className="w-10 text-right text-[10px] text-[var(--text-muted)]">{formatTime(progress)}</span>
          <input
            type="range"
            min={0}
            max={track.duration}
            value={progress}
            onChange={(event) => setProgress(Number(event.target.value))}
            className="flex-1 accent-[#005CFE]"
          />
          <span className="w-10 text-[10px] text-[var(--text-muted)]">{formatTime(track.duration)}</span>
        </div>
        <div className="flex items-center justify-center gap-4">
          <button onClick={previous} className="rounded-full p-2 hover:bg-[var(--bg-hover)]" title="Previous">
            <SkipBack size={19} />
          </button>
          <button
            onClick={() => setPlaying((value) => !value)}
            className="flex h-11 w-11 items-center justify-center rounded-full bg-[#005CFE] text-white"
            title={playing ? 'Pause' : 'Play'}
          >
            {playing ? <Pause size={20} fill="currentColor" /> : <Play size={20} fill="currentColor" />}
          </button>
          <button onClick={next} className="rounded-full p-2 hover:bg-[var(--bg-hover)]" title="Next">
            <SkipForward size={19} />
          </button>
          <div className="ml-4 hidden items-center gap-2 sm:flex">
            <Volume2 size={16} className="text-[var(--text-muted)]" />
            <input
              type="range"
              min={0}
              max={100}
              value={volume}
              onChange={(event) => setVolume(Number(event.target.value))}
              className="w-24 accent-[#005CFE]"
            />
          </div>
        </div>
      </footer>
    </div>
  );
}
