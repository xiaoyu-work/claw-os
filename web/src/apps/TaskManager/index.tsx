import { useState } from 'react';
import { Activity, BarChart3 } from 'lucide-react';
import ProcessesTab from './ProcessesTab';
import PerformanceTab from './PerformanceTab';

interface TaskManagerProps {
  windowId: string;
}

export default function TaskManager({ windowId }: TaskManagerProps) {
  const [activeTab, setActiveTab] = useState<'processes' | 'performance'>('processes');

  return (
    <div className="w-full h-full flex flex-col text-sm" style={{ background: 'var(--bg-workspace)' }}>
      {/* Tabs */}
      <div className="flex items-center gap-1 px-3 py-1 border-b" style={{ borderColor: 'rgba(0,0,0,0.06)', background: 'var(--bg-window)' }}>
        <button
          onClick={() => setActiveTab('processes')}
          className={`flex items-center gap-2 px-3 py-1.5 rounded text-sm transition-colors ${
            activeTab === 'processes' ? 'text-[var(--text-primary)]' : 'text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]'
          }`}
          style={activeTab === 'processes' ? { background: 'var(--bg-active)' } : {}}
        >
          <Activity size={16} />
          Processes
        </button>
        <button
          onClick={() => setActiveTab('performance')}
          className={`flex items-center gap-2 px-3 py-1.5 rounded text-sm transition-colors ${
            activeTab === 'performance' ? 'text-[var(--text-primary)]' : 'text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]'
          }`}
          style={activeTab === 'performance' ? { background: 'var(--bg-active)' } : {}}
        >
          <BarChart3 size={16} />
          Performance
        </button>
      </div>

      <div className="flex-1 overflow-hidden">
        {activeTab === 'processes' && <ProcessesTab />}
        {activeTab === 'performance' && <PerformanceTab />}
      </div>
    </div>
  );
}
