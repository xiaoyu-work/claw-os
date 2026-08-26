import { Monitor, Cpu, HardDrive, Globe } from 'lucide-react';

export default function AboutTab() {
  return (
    <div className="max-w-xl space-y-6">
      <div className="text-center py-4">
        <div className="w-20 h-20 mx-auto mb-3 rounded-2xl flex items-center justify-center" style={{ background: 'linear-gradient(135deg, #5A6670, #7D8B96)' }}>
          <span className="text-4xl font-bold text-white">W</span>
        </div>
        <h2 className="text-xl font-semibold">WebOS 24.04 LTS</h2>
        <p className="text-sm text-[var(--text-muted)]">Web-based Linux Desktop Environment</p>
      </div>

      <div className="space-y-2">
        {[
          { icon: Monitor, label: 'Desktop Environment', value: 'WebOS Desktop v1.0' },
          { icon: Cpu, label: 'Kernel', value: '6.5.0-webos-generic' },
          { icon: HardDrive, label: 'Memory', value: '16 GB DDR5' },
          { icon: Globe, label: 'Browser Engine', value: 'Chromium 120 (React 19)' },
          { icon: Monitor, label: 'Resolution', value: '1920 x 1080 @ 144Hz' },
          { icon: Cpu, label: 'Processor', value: 'WebOS vCPU (8 cores) @ 3.2GHz' },
          { icon: HardDrive, label: 'Storage', value: '2 TB NVMe SSD' },
        ].map((item, i) => (
          <div key={i} className="flex items-center justify-between p-3 rounded-lg" style={{ background: 'var(--bg-window)' }}>
            <div className="flex items-center gap-3">
              <item.icon size={18} className="text-[var(--accent-silver)]" />
              <span className="text-sm">{item.label}</span>
            </div>
            <span className="text-sm text-[var(--text-muted)]">{item.value}</span>
          </div>
        ))}
      </div>

      <div className="text-center text-xs text-[var(--text-muted)] pt-4">
        <p>Built with React 19 + TypeScript + Vite + Tailwind CSS</p>
        <p className="mt-1">WebOS Team</p>
      </div>
    </div>
  );
}
