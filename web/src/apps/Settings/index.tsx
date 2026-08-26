import { useState } from 'react';
import { Palette, Monitor, Wifi, Volume2, Info } from 'lucide-react';
import AppearanceTab from './AppearanceTab';
import DisplayTab from './DisplayTab';
import NetworkTab from './NetworkTab';
import SoundTab from './SoundTab';
import AboutTab from './AboutTab';

const tabs = [
  { id: 'appearance', name: 'Appearance', icon: Palette },
  { id: 'display', name: 'Display', icon: Monitor },
  { id: 'network', name: 'Network', icon: Wifi },
  { id: 'sound', name: 'Sound', icon: Volume2 },
  { id: 'about', name: 'About', icon: Info },
];

export default function Settings() {
  const [activeTab, setActiveTab] = useState('appearance');

  return (
    <div className="w-full h-full flex text-sm" style={{ background: 'var(--bg-workspace)' }}>
      {/* Sidebar */}
      <div className="w-14 shrink-0 py-2 sm:w-48" style={{ background: 'var(--bg-window)', borderRight: '1px solid rgba(0,0,0,0.06)' }}>
        {tabs.map((tab) => {
          const Icon = tab.icon;
          return (
            <button
              key={tab.id}
              onClick={() => setActiveTab(tab.id)}
              className={`w-full flex items-center justify-center gap-3 px-2 py-2.5 text-sm transition-colors sm:justify-start sm:px-4 ${
                activeTab === tab.id ? 'text-[var(--text-primary)]' : 'text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]'
              }`}
              style={activeTab === tab.id ? { background: 'var(--bg-active)' } : {}}
              title={tab.name}
            >
              <Icon size={18} />
              <span className="hidden sm:inline">{tab.name}</span>
            </button>
          );
        })}
      </div>

      {/* Content */}
      <div className="min-w-0 flex-1 overflow-y-auto p-3 sm:p-6">
        {activeTab === 'appearance' && <AppearanceTab />}
        {activeTab === 'display' && <DisplayTab />}
        {activeTab === 'network' && <NetworkTab />}
        {activeTab === 'sound' && <SoundTab />}
        {activeTab === 'about' && <AboutTab />}
      </div>
    </div>
  );
}
