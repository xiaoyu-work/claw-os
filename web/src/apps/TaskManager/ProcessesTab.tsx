import { useState, useEffect } from 'react';
import { X, ArrowUpDown } from 'lucide-react';

interface Process {
  pid: number;
  name: string;
  cpu: number;
  mem: number;
  status: 'Running' | 'Sleeping' | 'Waiting';
  user: string;
}

const initialProcesses: Process[] = [
  { pid: 1, name: 'systemd', cpu: 0.1, mem: 12.4, status: 'Running', user: 'root' },
  { pid: 234, name: 'webos-display', cpu: 1.8, mem: 45.2, status: 'Running', user: 'user' },
  { pid: 456, name: 'webos-compositor', cpu: 4.2, mem: 89.4, status: 'Running', user: 'user' },
  { pid: 567, name: 'webos-panel', cpu: 0.3, mem: 15.8, status: 'Sleeping', user: 'user' },
  { pid: 678, name: 'chromium-engine', cpu: 8.5, mem: 234.1, status: 'Running', user: 'user' },
  { pid: 789, name: 'terminal-service', cpu: 0.5, mem: 8.2, status: 'Running', user: 'user' },
  { pid: 890, name: 'file-manager', cpu: 1.2, mem: 32.6, status: 'Sleeping', user: 'user' },
  { pid: 901, name: 'pulseaudio', cpu: 0.2, mem: 6.8, status: 'Sleeping', user: 'user' },
  { pid: 2345, name: 'network-manager', cpu: 0.4, mem: 18.3, status: 'Running', user: 'root' },
  { pid: 3456, name: 'app-store', cpu: 2.1, mem: 56.7, status: 'Running', user: 'user' },
  { pid: 4567, name: 'settings-daemon', cpu: 0.1, mem: 9.4, status: 'Sleeping', user: 'user' },
  { pid: 5678, name: 'notification-svc', cpu: 0.0, mem: 4.2, status: 'Sleeping', user: 'user' },
  { pid: 6789, name: 'crash-handler', cpu: 0.0, mem: 3.1, status: 'Sleeping', user: 'root' },
  { pid: 7890, name: 'usb-handler', cpu: 0.1, mem: 5.6, status: 'Running', user: 'root' },
  { pid: 8901, name: 'bluetooth-svc', cpu: 0.3, mem: 11.2, status: 'Sleeping', user: 'user' },
  { pid: 9012, name: 'wifi-manager', cpu: 0.6, mem: 14.8, status: 'Running', user: 'root' },
  { pid: 1111, name: 'crypto-svc', cpu: 0.0, mem: 2.9, status: 'Sleeping', user: 'root' },
  { pid: 2222, name: 'backup-svc', cpu: 0.2, mem: 7.5, status: 'Waiting', user: 'user' },
  { pid: 3333, name: 'update-checker', cpu: 0.1, mem: 5.3, status: 'Sleeping', user: 'user' },
  { pid: 4444, name: 'printer-svc', cpu: 0.0, mem: 3.8, status: 'Sleeping', user: 'root' },
  { pid: 5555, name: 'calendar-svc', cpu: 0.2, mem: 8.9, status: 'Running', user: 'user' },
  { pid: 6666, name: 'email-svc', cpu: 0.5, mem: 22.1, status: 'Running', user: 'user' },
  { pid: 7777, name: 'music-player', cpu: 1.8, mem: 38.4, status: 'Running', user: 'user' },
  { pid: 8888, name: 'video-decoder', cpu: 6.2, mem: 156.3, status: 'Running', user: 'user' },
];

type SortKey = 'pid' | 'name' | 'cpu' | 'mem';

export default function ProcessesTab() {
  const [processes, setProcesses] = useState<Process[]>(initialProcesses);
  const [sortKey, setSortKey] = useState<SortKey>('pid');
  const [sortAsc, setSortAsc] = useState(true);
  const [selected, setSelected] = useState<number | null>(null);

  useEffect(() => {
    const interval = setInterval(() => {
      setProcesses((prev) => prev.map((p) => ({
        ...p,
        cpu: Math.max(0, Math.min(100, p.cpu + (Math.random() - 0.5) * 4)),
        mem: Math.max(0, p.mem + (Math.random() - 0.5) * 2),
      })));
    }, 2000);
    return () => clearInterval(interval);
  }, []);

  const handleSort = (key: SortKey) => {
    if (sortKey === key) setSortAsc(!sortAsc);
    else { setSortKey(key); setSortAsc(true); }
  };

  const sorted = [...processes].sort((a, b) => {
    const cmp = a[sortKey] > b[sortKey] ? 1 : -1;
    return sortAsc ? cmp : -cmp;
  });

  const handleKill = () => {
    if (!selected) return;
    setProcesses((prev) => prev.filter((p) => p.pid !== selected));
    setSelected(null);
  };

  return (
    <div className="w-full h-full flex flex-col">
      <div className="flex items-center justify-between px-3 py-2 border-b" style={{ borderColor: 'rgba(0,0,0,0.06)' }}>
        <span className="text-xs text-[var(--text-muted)]">{processes.length} processes</span>
        <button
          onClick={handleKill}
          disabled={!selected}
          className="flex items-center gap-1.5 px-3 py-1.5 rounded text-xs font-medium disabled:opacity-30 transition-colors hover:opacity-90"
          style={{ background: selected ? 'var(--error)' : 'var(--bg-input)', color: selected ? 'white' : 'var(--text-muted)' }}
        >
          <X size={14} />
          End Task
        </button>
      </div>
      <div className="flex-1 overflow-y-auto">
        <div className="grid text-xs font-medium text-[var(--text-muted)] px-3 py-1.5" style={{ gridTemplateColumns: '80px 1fr 80px 80px 100px 80px' }}>
          <button onClick={() => handleSort('pid')} className="flex items-center gap-1 hover:text-[var(--text-primary)]">PID <ArrowUpDown size={10} /></button>
          <button onClick={() => handleSort('name')} className="flex items-center gap-1 hover:text-[var(--text-primary)]">Name <ArrowUpDown size={10} /></button>
          <button onClick={() => handleSort('cpu')} className="flex items-center gap-1 hover:text-[var(--text-primary)]">CPU% <ArrowUpDown size={10} /></button>
          <button onClick={() => handleSort('mem')} className="flex items-center gap-1 hover:text-[var(--text-primary)]">Mem <ArrowUpDown size={10} /></button>
          <span>Status</span>
          <span>User</span>
        </div>
        {sorted.map((proc) => (
          <div
            key={proc.pid}
            onClick={() => setSelected(proc.pid)}
            className={`grid text-sm px-3 py-1 cursor-pointer transition-colors ${
              selected === proc.pid ? 'bg-[rgba(125,139,150,0.15)]' : 'hover:bg-[var(--bg-hover)]'
            }`}
            style={{ gridTemplateColumns: '80px 1fr 80px 80px 100px 80px' }}
          >
            <span className="font-mono text-[var(--text-secondary)]">{proc.pid}</span>
            <span className="text-[var(--text-primary)] truncate">{proc.name}</span>
            <span className="text-[var(--text-secondary)]">{proc.cpu.toFixed(1)}%</span>
            <span className="text-[var(--text-secondary)]">{proc.mem.toFixed(1)} MB</span>
            <span className={`text-xs ${proc.status === 'Running' ? 'text-[var(--success)]' : proc.status === 'Waiting' ? 'text-[var(--warning)]' : 'text-[var(--text-muted)]'}`}>{proc.status}</span>
            <span className="text-[var(--text-muted)] text-xs">{proc.user}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
