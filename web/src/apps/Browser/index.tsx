import { useMemo, useState, type FormEvent } from 'react';
import {
  ArrowLeft,
  ArrowRight,
  Globe,
  Home,
  LockKeyhole,
  MoreVertical,
  RotateCw,
  Star,
  X,
} from 'lucide-react';
import { publicAsset } from '@/lib/publicAsset';

export default function Browser() {
  const homeUrl = useMemo(
    () => new URL(publicAsset('site/index.html'), window.location.href).href,
    [],
  );
  const [history, setHistory] = useState([homeUrl]);
  const [historyIndex, setHistoryIndex] = useState(0);
  const [address, setAddress] = useState(homeUrl);
  const [frameKey, setFrameKey] = useState(0);
  const [loading, setLoading] = useState(true);
  const currentUrl = history[historyIndex];

  const normalizeAddress = (value: string) => {
    const trimmed = value.trim();
    if (!trimmed || trimmed === 'claw://home') return homeUrl;
    if (/^https?:\/\//i.test(trimmed)) return trimmed;
    if (trimmed.startsWith('/')) return new URL(trimmed, window.location.origin).href;
    return `https://${trimmed}`;
  };

  const navigate = (value: string) => {
    const nextUrl = normalizeAddress(value);
    const nextHistory = [...history.slice(0, historyIndex + 1), nextUrl];
    setHistory(nextHistory);
    setHistoryIndex(nextHistory.length - 1);
    setAddress(nextUrl);
    setLoading(true);
  };

  const handleSubmit = (event: FormEvent) => {
    event.preventDefault();
    navigate(address);
  };

  const goBack = () => {
    if (historyIndex === 0) return;
    const nextIndex = historyIndex - 1;
    setHistoryIndex(nextIndex);
    setAddress(history[nextIndex]);
    setLoading(true);
  };

  const goForward = () => {
    if (historyIndex === history.length - 1) return;
    const nextIndex = historyIndex + 1;
    setHistoryIndex(nextIndex);
    setAddress(history[nextIndex]);
    setLoading(true);
  };

  const goHome = () => {
    if (currentUrl === homeUrl) {
      setFrameKey((key) => key + 1);
      setLoading(true);
      return;
    }
    navigate(homeUrl);
  };

  return (
    <div className="flex h-full min-h-0 flex-col bg-[var(--bg-window)]">
      <div
        className="flex h-9 shrink-0 items-end gap-1 px-2 pt-1"
        style={{ background: 'var(--bg-workspace-alt)', borderBottom: '1px solid var(--border-medium)' }}
      >
        <div
          className="flex h-8 min-w-0 max-w-64 flex-1 items-center gap-2 rounded-t-md px-3 text-xs"
          style={{ background: 'var(--bg-window)' }}
        >
          <Globe size={14} className="shrink-0 text-[var(--accent-silver)]" />
          <span className="min-w-0 flex-1 truncate text-[var(--text-primary)]">Claw OS</span>
          <X size={13} className="shrink-0 text-[var(--text-muted)]" />
        </div>
        <button
          type="button"
          aria-label="New tab"
          className="mb-1 grid h-6 w-7 place-items-center rounded text-sm text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
        >
          +
        </button>
      </div>

      <div
        className="flex h-11 shrink-0 items-center gap-1.5 px-2"
        style={{ background: 'var(--bg-panel)', borderBottom: '1px solid var(--border-medium)' }}
      >
        <button
          type="button"
          aria-label="Back"
          disabled={historyIndex === 0}
          onClick={goBack}
          className="grid h-8 w-8 place-items-center rounded-md text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] disabled:opacity-30"
        >
          <ArrowLeft size={17} />
        </button>
        <button
          type="button"
          aria-label="Forward"
          disabled={historyIndex === history.length - 1}
          onClick={goForward}
          className="grid h-8 w-8 place-items-center rounded-md text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] disabled:opacity-30"
        >
          <ArrowRight size={17} />
        </button>
        <button
          type="button"
          aria-label="Reload"
          onClick={() => {
            setFrameKey((key) => key + 1);
            setLoading(true);
          }}
          className="grid h-8 w-8 place-items-center rounded-md text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
        >
          <RotateCw size={16} className={loading ? 'animate-spin' : ''} />
        </button>
        <button
          type="button"
          aria-label="Home"
          onClick={goHome}
          className="grid h-8 w-8 place-items-center rounded-md text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
        >
          <Home size={16} />
        </button>

        <form onSubmit={handleSubmit} className="flex min-w-0 flex-1">
          <div
            className="flex h-8 min-w-0 flex-1 items-center gap-2 rounded-md px-3"
            style={{ background: 'var(--bg-input)', border: '1px solid var(--border-medium)' }}
          >
            <LockKeyhole size={13} className="shrink-0 text-[var(--success)]" />
            <input
              aria-label="Address"
              value={address}
              onChange={(event) => setAddress(event.target.value)}
              onFocus={(event) => event.currentTarget.select()}
              className="min-w-0 flex-1 bg-transparent text-xs text-[var(--text-primary)] outline-none"
            />
            <button type="button" aria-label="Bookmark" className="text-[var(--text-muted)] hover:text-[var(--text-primary)]">
              <Star size={14} />
            </button>
          </div>
        </form>

        <button
          type="button"
          aria-label="Browser menu"
          className="grid h-8 w-8 place-items-center rounded-md text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
        >
          <MoreVertical size={17} />
        </button>
      </div>

      <div className="relative min-h-0 flex-1 bg-white">
        {loading && (
          <div className="absolute inset-x-0 top-0 z-10 h-0.5 overflow-hidden bg-[var(--bg-hover)]">
            <div className="h-full w-1/3 animate-pulse bg-[var(--accent-silver)]" />
          </div>
        )}
        <iframe
          key={`${currentUrl}-${frameKey}`}
          src={currentUrl}
          title="Claw OS website"
          className="h-full w-full border-0 bg-white"
          allow="clipboard-write; fullscreen"
          onLoad={() => setLoading(false)}
        />
      </div>
    </div>
  );
}
