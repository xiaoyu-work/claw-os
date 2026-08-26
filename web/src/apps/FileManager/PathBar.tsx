import { useFileSystemStore } from '@/stores/useFileSystemStore';

interface PathBarProps {
  path: string;
  onNavigate: (nodeId: string | null) => void;
}

export default function PathBar({ path, onNavigate }: PathBarProps) {
  const fs = useFileSystemStore.getState();
  const parts = path.split('/').filter(Boolean);
  return (
    <div className="flex items-center gap-0.5 h-7 px-2 rounded text-xs overflow-hidden"
      style={{ background: 'var(--bg-input)', border: '1px solid rgba(0,0,0,0.06)', color: 'var(--text-primary)' }}>
      <button onClick={() => onNavigate('fs-root')} className="hover:text-[var(--accent-silver)] transition-colors shrink-0">/</button>
      {parts.map((part, i) => {
        const partialPath = '/' + parts.slice(0, i + 1).join('/');
        const node = fs.getNodeByPath(partialPath);
        return (
          <span key={i} className="flex items-center shrink-0">
            <button
              onClick={() => onNavigate(node?.id || null)}
              className="hover:text-[var(--accent-silver)] transition-colors px-1 truncate max-w-[120px]"
            >
              {part}
            </button>
            {i < parts.length - 1 && <span className="text-[var(--text-muted)]">/</span>}
          </span>
        );
      })}
    </div>
  );
}
