import { Boxes, Cpu, Globe, Monitor, ShieldCheck, Sparkles } from 'lucide-react';
import { publicAsset } from '@/lib/publicAsset';

export default function AboutTab() {
  return (
    <div className="max-w-xl space-y-6">
      <div className="text-center py-4">
        <img
          src={publicAsset('app-icons/agent.svg')}
          alt="Claw OS"
          className="w-20 h-20 mx-auto mb-3"
        />
        <h2 className="text-xl font-semibold">Claw OS Demo System</h2>
        <p className="text-sm text-[var(--text-muted)]">Interactive preview of the agent-native operating system</p>
      </div>

      <div className="space-y-2">
        {[
          { icon: Monitor, label: 'Desktop', value: 'Claw OS Web Demo' },
          { icon: Sparkles, label: 'System Agent', value: 'Built in' },
          { icon: ShieldCheck, label: 'App permissions', value: 'Capability scoped' },
          { icon: Cpu, label: 'AI runtime', value: 'Shared system model gateway' },
          { icon: Boxes, label: 'App integration', value: 'Typed operations' },
          { icon: Globe, label: 'Demo runtime', value: 'React 19 + TypeScript' },
        ].map((item, i) => (
          <div key={i} className="flex flex-col items-start justify-between gap-1 p-3 rounded-lg sm:flex-row sm:items-center" style={{ background: 'var(--bg-window)' }}>
            <div className="flex items-center gap-3">
              <item.icon size={18} className="text-[var(--accent-silver)]" />
              <span className="text-sm">{item.label}</span>
            </div>
            <span className="break-words text-xs text-[var(--text-muted)] sm:text-right sm:text-sm">{item.value}</span>
          </div>
        ))}
      </div>

      <div className="text-center text-xs text-[var(--text-muted)] pt-4">
        <p>This demo mirrors the first-party Claw OS desktop surfaces.</p>
        <p className="mt-1">Claw OS · Apache-2.0</p>
      </div>
    </div>
  );
}
