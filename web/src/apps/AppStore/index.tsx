import { useMemo, useState } from 'react';
import { Check, RefreshCw, Search, ShieldCheck, Sparkles } from 'lucide-react';
import AppIcon from '@/components/AppIcon';
import { useAppRegistryStore } from '@/stores/useAppRegistryStore';
import { useWindowStore } from '@/stores/useWindowStore';
import StoreAiPanel from './StoreAiPanel';

export default function AppStore() {
  const apps = useAppRegistryStore((state) => state.apps);
  const openWindow = useWindowStore((state) => state.openWindow);
  const [query, setQuery] = useState('');
  const [category, setCategory] = useState('All');
  const [updates, setUpdates] = useState(() => new Set(['agent', 'store']));
  const [aiOpen, setAiOpen] = useState(false);

  const catalog = useMemo(
    () => Object.values(apps).filter((app) => app.id !== 'browser'),
    [apps],
  );
  const categories = ['All', ...Array.from(new Set(catalog.map((app) => app.category)))];
  const visibleApps = catalog.filter((app) => {
    const matchesCategory = category === 'All' || app.category === category;
    const text = `${app.name} ${app.description}`.toLowerCase();
    return matchesCategory && text.includes(query.toLowerCase());
  });

  const updateApp = (appId: string) => {
    setUpdates((current) => {
      const next = new Set(current);
      next.delete(appId);
      return next;
    });
  };

  const openRecommendedApp = (app: (typeof catalog)[number]) => {
    openWindow(app.id, app.name, {
      width: app.defaultWidth,
      height: app.defaultHeight,
    });
  };

  return (
    <div className="relative flex h-full min-h-0 flex-col text-sm" style={{ background: 'var(--bg-workspace)' }}>
      <header className="flex shrink-0 flex-wrap items-center gap-3 border-b px-4 py-3" style={{ background: 'var(--bg-window)', borderColor: 'rgba(0,0,0,0.06)' }}>
        <div>
          <h2 className="font-semibold text-[var(--text-primary)]">Claw OS App Store</h2>
          <p className="text-[11px] text-[var(--text-muted)]">First-party demo applications</p>
        </div>
        <button
          type="button"
          data-store-ai="toggle"
          onClick={() => setAiOpen((value) => !value)}
          className="flex h-9 shrink-0 items-center gap-1.5 rounded-lg px-3 text-xs font-medium text-white"
          style={{ background: '#005CFE' }}
          aria-label="Toggle App Finder"
          aria-expanded={aiOpen}
        >
          <Sparkles size={15} />
          Ask AI
        </button>
        <div className="relative ml-auto min-w-52 flex-1 sm:max-w-xs">
          <Search size={15} className="absolute left-3 top-1/2 -translate-y-1/2 text-[var(--text-muted)]" />
          <input
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="Search apps"
            className="h-9 w-full rounded-lg pl-9 pr-3 text-xs outline-none"
            style={{ background: 'var(--bg-input)', color: 'var(--text-primary)', border: '1px solid rgba(0,0,0,0.08)' }}
          />
        </div>
      </header>

      <div className="flex shrink-0 gap-2 overflow-x-auto border-b px-4 py-2" style={{ borderColor: 'rgba(0,0,0,0.06)' }}>
        {categories.map((item) => (
          <button
            key={item}
            onClick={() => setCategory(item)}
            className="shrink-0 rounded-full px-3 py-1.5 text-xs transition-colors"
            style={{
              background: category === item ? 'var(--accent-silver)' : 'var(--bg-input)',
              color: category === item ? '#fff' : 'var(--text-secondary)',
            }}
          >
            {item}
          </button>
        ))}
      </div>

      <div className="relative flex min-h-0 flex-1">
        <div className="min-w-0 flex-1 overflow-y-auto p-4">
          <div className="mb-4 rounded-xl border p-4" style={{ background: 'var(--bg-window)', borderColor: 'rgba(0,0,0,0.06)' }}>
          <div className="flex items-start gap-3">
            <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-[#005CFE]/10 text-[#005CFE]">
              <ShieldCheck size={21} />
            </div>
            <div>
              <h3 className="font-medium text-[var(--text-primary)]">Built for the Claw OS permission model</h3>
              <p className="mt-1 text-xs leading-relaxed text-[var(--text-muted)]">
                Every app declares its capabilities, while AI features use the shared system model instead of bundling a separate provider.
              </p>
            </div>
          </div>
        </div>

          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {visibleApps.map((app) => {
            const hasUpdate = updates.has(app.id);
            return (
              <article
                key={app.id}
                className="flex min-h-36 flex-col rounded-xl border p-4"
                style={{ background: 'var(--bg-window)', borderColor: 'rgba(0,0,0,0.06)' }}
              >
                <div className="flex items-start gap-3">
                  <AppIcon icon={app.icon} label={app.name} size={48} />
                  <div className="min-w-0">
                    <h3 className="truncate font-medium text-[var(--text-primary)]">{app.name}</h3>
                    <p className="text-[11px] text-[var(--text-muted)]">{app.category}</p>
                  </div>
                </div>
                <p className="mt-3 line-clamp-2 text-xs leading-relaxed text-[var(--text-secondary)]">
                  {app.description}
                </p>
                <button
                  onClick={() => hasUpdate && updateApp(app.id)}
                  className="mt-auto flex h-8 items-center justify-center gap-1.5 rounded-lg text-xs font-medium transition-colors"
                  style={{
                    background: hasUpdate ? 'var(--accent-silver)' : 'var(--bg-input)',
                    color: hasUpdate ? '#fff' : 'var(--text-muted)',
                  }}
                >
                  {hasUpdate ? <><RefreshCw size={13} /> Update</> : <><Check size={13} /> Installed</>}
                </button>
              </article>
            );
            })}
          </div>

          {visibleApps.length === 0 && (
            <div className="py-16 text-center text-sm text-[var(--text-muted)]">
              No apps match your search.
            </div>
          )}
        </div>
        <StoreAiPanel
          open={aiOpen}
          apps={catalog}
          onClose={() => setAiOpen(false)}
          onOpenApp={openRecommendedApp}
        />
      </div>
    </div>
  );
}
