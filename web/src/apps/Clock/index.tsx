import { useState, useEffect } from 'react';
import { Clock, Globe } from 'lucide-react';

interface ClockProps {
  windowId: string;
}

const timezones = [
  { label: 'UTC', offset: 0 },
  { label: 'New York', offset: -5 },
  { label: 'London', offset: 0 },
  { label: 'Tokyo', offset: 9 },
  { label: 'Sydney', offset: 11 },
  { label: 'Paris', offset: 1 },
  { label: 'Dubai', offset: 4 },
  { label: 'Singapore', offset: 8 },
];

export default function ClockApp({ windowId }: ClockProps) {
  const [time, setTime] = useState(new Date());
  const [selectedTz, setSelectedTz] = useState(0);

  useEffect(() => {
    const interval = setInterval(() => setTime(new Date()), 1000);
    return () => clearInterval(interval);
  }, []);

  const tz = timezones[selectedTz];
  const utc = time.getTime() + time.getTimezoneOffset() * 60000;
  const tzTime = new Date(utc + tz.offset * 3600000);

  const hours = tzTime.getHours();
  const minutes = tzTime.getMinutes();
  const seconds = tzTime.getSeconds();

  const hourDeg = ((hours % 12) / 12) * 360 + (minutes / 60) * 30;
  const minuteDeg = (minutes / 60) * 360 + (seconds / 60) * 6;
  const secondDeg = (seconds / 60) * 360;

  const pad = (n: number) => String(n).padStart(2, '0');

  return (
    <div className="w-full h-full flex flex-col items-center justify-center p-4" style={{ background: 'var(--bg-workspace)' }}>
      {/* Digital display */}
      <div className="text-5xl font-mono font-light text-[var(--text-primary)] mb-2" style={{ letterSpacing: '0.05em' }}>
        {pad(hours)}:{pad(minutes)}:{pad(seconds)}
      </div>
      <div className="text-sm text-[var(--text-secondary)] mb-6">
        {tzTime.toLocaleDateString('en-US', { weekday: 'long', year: 'numeric', month: 'long', day: 'numeric' })}
      </div>
      <div className="text-xs text-[var(--accent-silver)] mb-6">{tz.label} (UTC{tz.offset >= 0 ? '+' : ''}{tz.offset})</div>

      {/* Analog clock */}
      <div className="relative w-48 h-48 mb-6">
        <svg viewBox="0 0 200 200" className="w-full h-full">
          {/* Clock face */}
          <circle cx="100" cy="100" r="95" fill="none" stroke="rgba(0,0,0,0.12)" strokeWidth="2" />
          <circle cx="100" cy="100" r="92" fill="none" stroke="rgba(0,0,0,0.04)" strokeWidth="1" />

          {/* Hour markers */}
          {Array.from({ length: 12 }, (_, i) => {
            const angle = (i * 30 - 90) * (Math.PI / 180);
            const x1 = 100 + 78 * Math.cos(angle);
            const y1 = 100 + 78 * Math.sin(angle);
            const x2 = 100 + 85 * Math.cos(angle);
            const y2 = 100 + 85 * Math.sin(angle);
            return <line key={i} x1={x1} y1={y1} x2={x2} y2={y2} stroke="var(--text-secondary)" strokeWidth={i % 3 === 0 ? 3 : 1} />;
          })}

          {/* Hour hand */}
          <line x1="100" y1="100" x2={100 + 50 * Math.cos((hourDeg - 90) * Math.PI / 180)} y2={100 + 50 * Math.sin((hourDeg - 90) * Math.PI / 180)} stroke="var(--text-primary)" strokeWidth="4" strokeLinecap="round" />

          {/* Minute hand */}
          <line x1="100" y1="100" x2={100 + 70 * Math.cos((minuteDeg - 90) * Math.PI / 180)} y2={100 + 70 * Math.sin((minuteDeg - 90) * Math.PI / 180)} stroke="var(--text-primary)" strokeWidth="2.5" strokeLinecap="round" />

          {/* Second hand */}
          <line x1="100" y1="100" x2={100 + 75 * Math.cos((secondDeg - 90) * Math.PI / 180)} y2={100 + 75 * Math.sin((secondDeg - 90) * Math.PI / 180)} stroke="var(--accent-silver)" strokeWidth="1" strokeLinecap="round" />

          {/* Center dot */}
          <circle cx="100" cy="100" r="4" fill="var(--accent-silver)" />
          <circle cx="100" cy="100" r="2" fill="var(--bg-workspace)" />
        </svg>
      </div>

      {/* Timezone selector */}
      <div className="flex flex-wrap justify-center gap-2 max-w-md">
        {timezones.map((tz, i) => (
          <button
            key={tz.label}
            onClick={() => setSelectedTz(i)}
            className={`flex items-center gap-1 px-3 py-1.5 rounded-full text-xs transition-colors ${
              selectedTz === i ? 'text-white' : 'text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]'
            }`}
            style={selectedTz === i ? { background: 'var(--accent-dark-gray)' } : { background: 'var(--bg-input)' }}
          >
            <Globe size={12} />
            {tz.label}
          </button>
        ))}
      </div>
    </div>
  );
}
