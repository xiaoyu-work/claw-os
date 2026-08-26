import { useState, useEffect, useMemo } from 'react';
import { useSystemStore } from '@/stores/useSystemStore';

export default function PerformanceTab() {
  const [cpuHistory, setCpuHistory] = useState<number[]>(Array(30).fill(15));
  const [memUsage, setMemUsage] = useState(25);
  const uptime = '2h 15m';

  useEffect(() => {
    const interval = setInterval(() => {
      setCpuHistory((prev) => {
        const next = [...prev.slice(1), Math.floor(5 + Math.random() * 40)];
        return next;
      });
      setMemUsage(20 + Math.floor(Math.random() * 15));
    }, 2000);
    return () => clearInterval(interval);
  }, []);

  const maxCpu = Math.max(...cpuHistory, 1);
  const points = cpuHistory.map((v, i) => `${(i / 29) * 100},${100 - (v / 60) * 100}`).join(' ');

  return (
    <div className="w-full h-full overflow-y-auto p-4 space-y-4">
      {/* CPU */}
      <div className="rounded-xl p-4" style={{ background: 'var(--bg-window)' }}>
        <div className="flex items-center justify-between mb-3">
          <div className="flex items-center gap-2">
            <div className="w-8 h-8 rounded-lg flex items-center justify-center" style={{ background: 'var(--bg-input)' }}>
              <span className="text-sm font-bold text-[var(--accent-silver)]">CPU</span>
            </div>
            <div>
              <div className="text-sm font-medium">WebOS vCPU (8)</div>
              <div className="text-xs text-[var(--text-muted)]">@ 3.2 GHz</div>
            </div>
          </div>
          <div className="text-2xl font-light text-[var(--accent-silver)]">{cpuHistory[cpuHistory.length - 1]}%</div>
        </div>
        <svg viewBox="0 0 100 100" className="w-full h-32" preserveAspectRatio="none">
          <defs>
            <linearGradient id="cpuGrad" x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor="#7D8B96" stopOpacity="0.5" />
              <stop offset="100%" stopColor="#7D8B96" stopOpacity="0.05" />
            </linearGradient>
          </defs>
          <polygon points={`0,100 ${points} 100,100`} fill="url(#cpuGrad)" />
          <polyline points={points} fill="none" stroke="#7D8B96" strokeWidth="0.5" />
          <line x1="0" y1="50" x2="100" y2="50" stroke="rgba(0,0,0,0.06)" strokeWidth="0.3" />
          <line x1="0" y1="25" x2="100" y2="25" stroke="rgba(0,0,0,0.06)" strokeWidth="0.3" />
          <line x1="0" y1="75" x2="100" y2="75" stroke="rgba(0,0,0,0.06)" strokeWidth="0.3" />
        </svg>
      </div>

      {/* Memory */}
      <div className="rounded-xl p-4" style={{ background: 'var(--bg-window)' }}>
        <div className="flex items-center justify-between mb-3">
          <div className="flex items-center gap-2">
            <div className="w-8 h-8 rounded-lg flex items-center justify-center" style={{ background: 'var(--bg-input)' }}>
              <span className="text-sm font-bold text-[var(--accent-pink)]">M</span>
            </div>
            <div>
              <div className="text-sm font-medium">Memory</div>
              <div className="text-xs text-[var(--text-muted)]">16 GB DDR5</div>
            </div>
          </div>
          <div className="text-2xl font-light text-[var(--accent-pink)]">{memUsage}%</div>
        </div>
        <div className="w-full h-4 rounded-full overflow-hidden" style={{ background: 'var(--bg-input)' }}>
          <div className="h-full rounded-full transition-all" style={{ width: `${memUsage}%`, background: 'linear-gradient(90deg, #EC4899, #F472B6)' }} />
        </div>
        <div className="flex justify-between mt-1 text-xs text-[var(--text-muted)]">
          <span>{(memUsage * 16 / 100).toFixed(1)} GB used</span>
          <span>{(16 - memUsage * 16 / 100).toFixed(1)} GB available</span>
        </div>
      </div>

      {/* Uptime */}
      <div className="rounded-xl p-4" style={{ background: 'var(--bg-window)' }}>
        <div className="flex items-center justify-between">
          <span className="text-sm text-[var(--text-secondary)]">System Uptime</span>
          <span className="text-lg font-mono text-[var(--text-primary)]">{uptime}</span>
        </div>
      </div>
    </div>
  );
}
