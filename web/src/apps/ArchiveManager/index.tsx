import { useState, useCallback } from 'react';
import { FolderOpen, FilePlus, Package, PackageOpen, FileArchive, Trash2, ChevronRight, Folder } from 'lucide-react';

interface ArchiveEntry { name: string; type: 'file' | 'folder'; size: number; date: string; items?: ArchiveEntry[] }
interface Archive { id: string; name: string; entries: ArchiveEntry[]; createdAt: string; size: number }

function generateId() { return Math.random().toString(36).substring(2, 10); }
function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

const DEMO_ARCHIVES: Archive[] = [
  {
    id: generateId(), name: 'documents.zip', createdAt: '2024-01-15', size: 245760,
    entries: [
      { name: 'readme.txt', type: 'file', size: 1024, date: '2024-01-15' },
      { name: 'src', type: 'folder', size: 0, date: '2024-01-15', items: [
        { name: 'index.ts', type: 'file', size: 2048, date: '2024-01-14' },
        { name: 'utils.ts', type: 'file', size: 512, date: '2024-01-13' },
      ]},
      { name: 'package.json', type: 'file', size: 256, date: '2024-01-15' },
    ],
  },
  {
    id: generateId(), name: 'photos.zip', createdAt: '2024-02-20', size: 5242880,
    entries: [
      { name: 'vacation', type: 'folder', size: 0, date: '2024-02-20', items: [
        { name: 'beach.jpg', type: 'file', size: 1048576, date: '2024-02-18' },
        { name: 'mountain.jpg', type: 'file', size: 2097152, date: '2024-02-19' },
      ]},
      { name: 'profile.png', type: 'file', size: 512000, date: '2024-02-20' },
    ],
  },
];

function EntryTree({ entries, depth = 0 }: { entries: ArchiveEntry[]; depth?: number }) {
  const [expanded, setExpanded] = useState<Set<string>>(new Set());

  const toggle = useCallback((name: string) => {
    setExpanded(prev => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name); else next.add(name);
      return next;
    });
  }, []);

  return (
    <div className="flex flex-col">
      {entries.map(entry => (
        <div key={entry.name}>
          <button onClick={() => entry.type === 'folder' ? toggle(entry.name) : undefined}
            className="flex items-center gap-1 py-0.5 text-xs w-full text-left hover:bg-[var(--bg-hover)] rounded px-1 transition-all"
            style={{ paddingLeft: `${depth * 16 + 4}px`, color: 'var(--text-secondary)' }}>
            {entry.type === 'folder' ? (
              <>
                <ChevronRight size={10} className={`transition-transform ${expanded.has(entry.name) ? 'rotate-90' : ''}`} />
                <Folder size={10} style={{ color: 'var(--accent-silver)' }} />
              </>
            ) : (
              <>
                <span className="w-[10px]" />
                <FileArchive size={10} style={{ color: 'var(--text-muted)' }} />
              </>
            )}
            <span className="truncate flex-1">{entry.name}</span>
            <span className="text-[10px] shrink-0" style={{ color: 'var(--text-muted)' }}>{formatSize(entry.size)}</span>
          </button>
          {entry.type === 'folder' && entry.items && expanded.has(entry.name) && (
            <EntryTree entries={entry.items} depth={depth + 1} />
          )}
        </div>
      ))}
    </div>
  );
}

export default function ArchiveManager({ windowId: _windowId }: { windowId: string }) {
  const [archives, setArchives] = useState<Archive[]>(DEMO_ARCHIVES);
  const [selected, setSelected] = useState<string | null>(DEMO_ARCHIVES[0]?.id || null);
  const [showCreate, setShowCreate] = useState(false);
  const [newName, setNewName] = useState('');

  const selectedArchive = archives.find(a => a.id === selected);

  const createArchive = useCallback(() => {
    if (!newName.trim()) return;
    const archive: Archive = { id: generateId(), name: newName.endsWith('.zip') ? newName : `${newName}.zip`, entries: [], createdAt: new Date().toISOString().split('T')[0], size: 0 };
    setArchives(prev => [...prev, archive]);
    setSelected(archive.id);
    setNewName('');
    setShowCreate(false);
  }, [newName]);

  const deleteArchive = useCallback((id: string) => {
    setArchives(prev => prev.filter(a => a.id !== id));
    if (selected === id) setSelected(null);
  }, [selected]);

  return (
    <div className="w-full h-full flex" style={{ background: 'var(--bg-workspace)' }}>
      {/* Archive list */}
      <div className="w-48 flex flex-col border-r" style={{ borderColor: 'rgba(0,0,0,0.06)', background: 'var(--bg-workspace)' }}>
        <div className="flex items-center justify-between p-2 border-b" style={{ borderColor: 'rgba(0,0,0,0.06)' }}>
          <span className="text-xs font-medium" style={{ color: 'var(--text-secondary)' }}>Archives</span>
          <button onClick={() => setShowCreate(!showCreate)} className="p-1 rounded hover:bg-[var(--bg-hover)]" style={{ color: 'var(--accent-silver)' }}><FilePlus size={14} /></button>
        </div>

        {showCreate && (
          <div className="p-2 border-b" style={{ borderColor: 'rgba(0,0,0,0.06)' }}>
            <input value={newName} onChange={e => setNewName(e.target.value)} onKeyDown={e => e.key === 'Enter' && createArchive()} placeholder="Archive name..."
              className="w-full text-xs px-2 py-1 rounded outline-none mb-1" style={{ background: 'var(--bg-input)', color: 'var(--text-primary)', border: '1px solid rgba(0,0,0,0.06)' }} autoFocus />
            <button onClick={createArchive} className="w-full py-1 rounded text-[10px] font-medium text-white" style={{ background: 'var(--accent-dark-gray)' }}>Create</button>
          </div>
        )}

        <div className="flex-1 overflow-y-auto">
          {archives.map(a => (
            <button key={a.id} onClick={() => setSelected(a.id)}
              className="w-full text-left p-2 flex items-center gap-2 hover:bg-[var(--bg-hover)] transition-all border-b"
              style={{ borderColor: 'rgba(0,0,0,0.04)', background: selected === a.id ? 'rgba(125,139,150,0.1)' : 'transparent' }}>
              <Package size={14} style={{ color: 'var(--accent-silver)' }} />
              <div className="flex-1 min-w-0">
                <div className="text-xs truncate" style={{ color: 'var(--text-primary)' }}>{a.name}</div>
                <div className="text-[10px]" style={{ color: 'var(--text-muted)' }}>{formatSize(a.size)} | {a.entries.length} items</div>
              </div>
              <button onClick={(e) => { e.stopPropagation(); deleteArchive(a.id); }} className="p-0.5 rounded hover:bg-[var(--bg-hover)]" style={{ color: 'var(--text-muted)' }}><Trash2 size={10} /></button>
            </button>
          ))}
        </div>
      </div>

      {/* Detail view */}
      <div className="flex-1 flex flex-col">
        {selectedArchive ? (
          <>
            <div className="flex items-center justify-between p-3 border-b" style={{ borderColor: 'rgba(0,0,0,0.06)' }}>
              <div className="flex items-center gap-2">
                <PackageOpen size={16} style={{ color: 'var(--accent-silver)' }} />
                <span className="text-sm font-medium" style={{ color: 'var(--text-primary)' }}>{selectedArchive.name}</span>
              </div>
              <div className="text-xs" style={{ color: 'var(--text-muted)' }}>{formatSize(selectedArchive.size)} | {selectedArchive.createdAt}</div>
            </div>
            <div className="flex-1 overflow-auto p-2">
              {selectedArchive.entries.length === 0 ? (
                <div className="flex flex-col items-center justify-center h-full">
                  <FolderOpen size={24} style={{ color: 'var(--text-muted)' }} />
                  <p className="text-xs mt-2" style={{ color: 'var(--text-muted)' }}>Empty archive</p>
                </div>
              ) : (
                <EntryTree entries={selectedArchive.entries} />
              )}
            </div>
          </>
        ) : (
          <div className="flex-1 flex flex-col items-center justify-center">
            <Package size={32} style={{ color: 'var(--text-muted)' }} />
            <p className="text-xs mt-2" style={{ color: 'var(--text-muted)' }}>Select an archive to view contents</p>
          </div>
        )}
      </div>
    </div>
  );
}
