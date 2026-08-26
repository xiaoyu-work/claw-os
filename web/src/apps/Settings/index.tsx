import { useState } from 'react';
import { Palette, Monitor, Wifi, Volume2, Users, Info } from 'lucide-react';
import AppearanceTab from './AppearanceTab';
import DisplayTab from './DisplayTab';
import NetworkTab from './NetworkTab';
import SoundTab from './SoundTab';
import UsersTab from './UsersTab';
import AboutTab from './AboutTab';

interface SettingsProps {
  windowId: string;
}

const tabs = [
  { id: 'appearance', name: 'Appearance', icon: Palette },
  { id: 'display', name: 'Display', icon: Monitor },
  { id: 'network', name: 'Network', icon: Wifi },
  { id: 'sound', name: 'Sound', icon: Volume2 },
  { id: 'users', name: 'Users', icon: Users },
  { id: 'about', name: 'About', icon: Info },
];

export default function Settings({ windowId }: SettingsProps) {
  const [activeTab, setActiveTab] = useState('appearance');

  return (
    <div className="w-full h-full flex text-sm" style={{ background: 'var(--bg-workspace)' }}>
      {/* Sidebar */}
      <div className="w-48 shrink-0 py-2" style={{ background: 'var(--bg-window)', borderRight: '1px solid rgba(0,0,0,0.06)' }}>
        {tabs.map((tab) => {
          const Icon = tab.icon;
          return (
            <button
              key={tab.id}
              onClick={() => setActiveTab(tab.id)}
              className={`w-full flex items-center gap-3 px-4 py-2.5 text-sm transition-colors ${
                activeTab === tab.id ? 'text-[var(--text-primary)]' : 'text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]'
              }`}
              style={activeTab === tab.id ? { background: 'var(--bg-active)' } : {}}
            >
              <Icon size={18} />
              {tab.name}
            </button>
          );
        })}
      </div>

      {/* Content */}
      <div className="flex-1 p-6 overflow-y-auto">
        {activeTab === 'appearance' && <AppearanceTab />}
        {activeTab === 'display' && <DisplayTab />}
        {activeTab === 'network' && <NetworkTab />}
        {activeTab === 'sound' && <SoundTab />}
        {activeTab === 'users' && <UsersTab />}
        {activeTab === 'about' && <AboutTab />}
      </div>
    </div>
  );
}
