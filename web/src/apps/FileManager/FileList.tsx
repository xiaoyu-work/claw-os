import { Folder, FileText, Image, Music, Film, FileArchive, FileCode, File } from 'lucide-react';
import type { FSNode } from '@/stores/useFileSystemStore';

interface FileListProps {
  nodes: FSNode[];
  viewMode: 'list' | 'grid';
  selectedNode: string | null;
  onSelect: (id: string) => void;
  onDoubleClick: (node: FSNode) => void;
  onContextMenu: (e: React.MouseEvent, node: FSNode) => void;
  renaming: string | null;
  renameValue: string;
  setRenameValue: (val: string) => void;
  onRenameSubmit: () => void;
}

function getFileIcon(node: FSNode) {
  if (node.type === 'directory') return <Folder size={20} className="text-[var(--accent-silver)]" />;
  const mime = node.mimeType || '';
  const name = node.name.toLowerCase();
  if (mime.startsWith('image/') || name.endsWith('.png') || name.endsWith('.jpg')) return <Image size={20} className="text-[var(--info)]" />;
  if (mime.startsWith('audio/') || name.endsWith('.mp3')) return <Music size={20} className="text-[var(--accent-pink)]" />;
  if (mime.startsWith('video/') || name.endsWith('.mp4')) return <Film size={20} className="text-[var(--warning)]" />;
  if (name.endsWith('.zip') || name.endsWith('.tar')) return <FileArchive size={20} className="text-[var(--success)]" />;
  if (name.endsWith('.js') || name.endsWith('.ts') || name.endsWith('.tsx') || name.endsWith('.json')) return <FileCode size={20} className="text-[var(--warning)]" />;
  if (name.endsWith('.txt') || name.endsWith('.md')) return <FileText size={20} className="text-[var(--text-secondary)]" />;
  return <File size={20} className="text-[var(--text-muted)]" />;
}

function formatSize(size: number): string {
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
  return `${(size / 1024 / 1024).toFixed(1)} MB`;
}

export default function FileList({
  nodes, viewMode, selectedNode, onSelect, onDoubleClick, onContextMenu,
  renaming, renameValue, setRenameValue, onRenameSubmit,
}: FileListProps) {
  if (viewMode === 'grid') {
    return (
      <div className="grid grid-cols-4 sm:grid-cols-6 md:grid-cols-8 gap-1 p-2">
        {nodes.map((node) => (
          <button
            key={node.id}
            className={`flex flex-col items-center gap-1 p-3 rounded-lg transition-all ${
              selectedNode === node.id ? 'bg-[rgba(125,139,150,0.2)]' : 'hover:bg-[var(--bg-hover)]'
            }`}
            onClick={(event) => {
              event.stopPropagation();
              onSelect(node.id);
            }}
            onDoubleClick={() => onDoubleClick(node)}
            onContextMenu={(e) => onContextMenu(e, node)}
          >
            {getFileIcon(node)}
            {renaming === node.id ? (
              <input
                value={renameValue}
                onChange={(e) => setRenameValue(e.target.value)}
                onKeyDown={(e) => { if (e.key === 'Enter') onRenameSubmit(); if (e.key === 'Escape') onRenameSubmit(); }}
                onBlur={onRenameSubmit}
                autoFocus
                className="w-full text-center text-xs bg-[var(--bg-input)] rounded px-1 outline-none"
                onClick={(e) => e.stopPropagation()}
              />
            ) : (
              <span className="text-xs text-[var(--text-primary)] text-center truncate w-full">{node.name}</span>
            )}
          </button>
        ))}
      </div>
    );
  }

  return (
    <div className="w-full">
      <div
        className="grid grid-cols-[24px_minmax(0,1fr)] px-3 py-1.5 text-xs font-medium text-[var(--text-muted)] border-b sm:grid-cols-[24px_minmax(0,1fr)_80px_100px_120px_80px]"
        style={{ borderColor: 'rgba(0,0,0,0.06)', background: 'var(--bg-window)' }}
      >
        <span></span>
        <span>Name</span>
        <span className="hidden sm:block">Size</span>
        <span className="hidden sm:block">Type</span>
        <span className="hidden sm:block">Modified</span>
        <span className="hidden sm:block">Owner</span>
      </div>
      {nodes.map((node) => (
        <div
          key={node.id}
          className={`grid grid-cols-[24px_minmax(0,1fr)] px-3 py-1.5 text-sm items-center cursor-pointer transition-colors sm:grid-cols-[24px_minmax(0,1fr)_80px_100px_120px_80px] ${
            selectedNode === node.id ? 'bg-[rgba(125,139,150,0.15)]' : 'hover:bg-[var(--bg-hover)]'
          }`}
          onClick={(event) => {
            event.stopPropagation();
            onSelect(node.id);
          }}
          onDoubleClick={() => onDoubleClick(node)}
          onContextMenu={(e) => onContextMenu(e, node)}
        >
          <span>{getFileIcon(node)}</span>
          <span className="text-[var(--text-primary)] truncate">
            {renaming === node.id ? (
              <input
                value={renameValue}
                onChange={(e) => setRenameValue(e.target.value)}
                onKeyDown={(e) => { if (e.key === 'Enter') onRenameSubmit(); if (e.key === 'Escape') onRenameSubmit(); }}
                onBlur={onRenameSubmit}
                autoFocus
                className="w-full bg-[var(--bg-input)] rounded px-1 outline-none"
                onClick={(e) => e.stopPropagation()}
              />
            ) : node.name}
          </span>
          <span className="hidden text-[var(--text-muted)] text-xs sm:block">{node.type === 'directory' ? '--' : formatSize(node.size || 0)}</span>
          <span className="hidden text-[var(--text-muted)] text-xs sm:block">{node.type === 'directory' ? 'Folder' : (node.mimeType?.split('/')[1] || 'File')}</span>
          <span className="hidden text-[var(--text-muted)] text-xs sm:block">{node.modifiedAt.toLocaleDateString()}</span>
          <span className="hidden text-[var(--text-muted)] text-xs sm:block">{node.owner}</span>
        </div>
      ))}
    </div>
  );
}
