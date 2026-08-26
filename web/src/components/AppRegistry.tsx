import type { ComponentType } from 'react';
import type { ReactNode } from 'react';

import FileManager from '@/apps/FileManager';
import Settings from '@/apps/Settings';
import TextEditor from '@/apps/TextEditor';
import Browser from '@/apps/Browser';
import Agent from '@/apps/Agent';
import AppStore from '@/apps/AppStore';
import MediaPlayer from '@/apps/MediaPlayer';
import Screenshot from '@/apps/Screenshot';

// Map app IDs to their component implementations
const appComponents: Record<string, ComponentType<{ windowId: string }>> = {
  agent: Agent,
  filemanager: FileManager,
  texteditor: TextEditor,
  store: AppStore,
  player: MediaPlayer,
  screenshot: Screenshot,
  settings: Settings,
  browser: Browser,
};

// Placeholder for apps not yet implemented
function AppPlaceholder({ appId }: { appId: string }) {
  return (
    <div className="w-full h-full flex flex-col items-center justify-center text-sm" style={{ background: 'var(--bg-workspace)' }}>
      <div className="w-16 h-16 rounded-2xl flex items-center justify-center mb-4" style={{ background: 'var(--bg-input)' }}>
        <span className="text-2xl text-[var(--accent-silver)]">?</span>
      </div>
      <h3 className="text-base font-medium text-[var(--text-primary)] mb-1">Coming Soon</h3>
      <p className="text-xs text-[var(--text-muted)]">This application will be available in a future update.</p>
      <p className="text-[10px] text-[var(--text-muted)] mt-2">App ID: {appId}</p>
    </div>
  );
}

export function getAppComponent(appId: string): ComponentType<{ windowId: string }> {
  return appComponents[appId] || (() => <AppPlaceholder appId={appId} />);
}

export function renderApp(appId: string, windowId: string): ReactNode {
  const Component = getAppComponent(appId);
  return <Component key={windowId} windowId={windowId} />;
}
