import { useState, useRef, useEffect, useCallback } from 'react';
import { FilePlus, FolderOpen, Save, SaveAll, Scissors, Copy, ClipboardPaste, Search, WrapText } from 'lucide-react';
import { useFileSystemStore, type FSNode } from '@/stores/useFileSystemStore';

interface TextEditorProps {
  windowId: string;
}

export default function TextEditor({ windowId: _windowId }: TextEditorProps) {
  const [content, setContent] = useState('');
  const [fileName, setFileName] = useState('Untitled.txt');
  const [wordWrap, setWordWrap] = useState(true);
  const [findOpen, setFindOpen] = useState(false);
  const [findText, setFindText] = useState('');
  const [replaceText, setReplaceText] = useState('');
  const [fontSize, setFontSize] = useState(14);
  const [currentFileId, setCurrentFileId] = useState<string | null>(null);
  const [showOpenDialog, setShowOpenDialog] = useState(false);
  const [showSaveDialog, setShowSaveDialog] = useState(false);
  const [saveName, setSaveName] = useState('');
  const [openFiles, setOpenFiles] = useState<FSNode[]>([]);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const nodes = useFileSystemStore((s) => s.nodes);
  const getChildren = useFileSystemStore((s) => s.getChildren);
  const currentDirectory = useFileSystemStore((s) => s.currentDirectory);
  const readFile = useFileSystemStore((s) => s.readFile);
  const writeFile = useFileSystemStore((s) => s.writeFile);
  const createFile = useFileSystemStore((s) => s.createFile);

  // Check for pending file open from FileManager double-click
  useEffect(() => {
    const pending = (window as any).__pendingFileOpen;
    if (pending && pending.appId === 'texteditor') {
      const node = nodes.find((n) => n.id === pending.fileId);
      if (node && node.type === 'file') {
        const data = readFile(pending.fileId);
        setContent(data || '');
        setFileName(pending.fileName || node.name);
        setCurrentFileId(pending.fileId);
      }
      (window as any).__pendingFileOpen = null;
    }
  }, [nodes, readFile]);

  const lines = content.split('\n').length;
  const chars = content.length;

  const handleNew = () => { setContent(''); setFileName('Untitled.txt'); setCurrentFileId(null); };

  const handleOpen = (nodeId: string) => {
    const fs = useFileSystemStore.getState();
    const node = fs.getNode(nodeId);
    if (!node || node.type !== 'file') return;
    const data = readFile(nodeId);
    setContent(data || '');
    setFileName(node.name);
    setCurrentFileId(nodeId);
    setShowOpenDialog(false);
  };

  const handleSave = () => {
    if (currentFileId) {
      writeFile(currentFileId, content);
    } else {
      setSaveName(fileName);
      setShowSaveDialog(true);
    }
  };

  const handleSaveAs = () => {
    if (!saveName.trim()) return;
    const fs = useFileSystemStore.getState();
    fs.createFile(saveName, currentDirectory, content);
    setFileName(saveName);
    setShowSaveDialog(false);
  };

  const handleFindReplace = useCallback(() => {
    if (!findText) return;
    if (replaceText) {
      setContent(content.split(findText).join(replaceText));
    }
  }, [content, findText, replaceText]);

  const handleSelectAll = () => { textareaRef.current?.select(); };

  const handleCut = () => {
    textareaRef.current?.setRangeText('', textareaRef.current.selectionStart, textareaRef.current.selectionEnd, 'end');
    setContent(textareaRef.current?.value || content);
  };

  const handleCopy = () => { navigator.clipboard.writeText(textareaRef.current?.value.slice(textareaRef.current.selectionStart, textareaRef.current.selectionEnd) || ''); };

  const handlePaste = async () => {
    const text = await navigator.clipboard.readText();
    textareaRef.current?.setRangeText(text, textareaRef.current.selectionStart, textareaRef.current.selectionEnd, 'end');
    setContent(textareaRef.current?.value || content);
  };

  // Line numbers
  const lineNumbers = Array.from({ length: Math.max(lines, 1) }, (_, i) => i + 1);

  return (
    <div className="w-full h-full flex flex-col text-sm" style={{ background: 'var(--bg-workspace)' }}>
      {/* Menu bar */}
      <div className="flex items-center gap-0.5 px-2 py-1 border-b" style={{ background: 'var(--bg-window)', borderColor: 'rgba(0,0,0,0.06)' }}>
        <div className="relative group">
          <button className="px-2 py-1 rounded text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]">File</button>
          <div className="absolute top-full left-0 w-40 py-1 rounded-lg hidden group-hover:block z-50" style={{ background: 'var(--bg-panel)', boxShadow: 'var(--shadow-md)', border: '1px solid rgba(0,0,0,0.10)' }}>
            <button onClick={handleNew} className="w-full text-left px-3 py-1.5 text-sm text-[var(--text-primary)] hover:bg-[var(--bg-hover)] flex items-center gap-2"><FilePlus size={14} />New</button>
            <button onClick={() => setShowOpenDialog(true)} className="w-full text-left px-3 py-1.5 text-sm text-[var(--text-primary)] hover:bg-[var(--bg-hover)] flex items-center gap-2"><FolderOpen size={14} />Open</button>
            <button onClick={handleSave} className="w-full text-left px-3 py-1.5 text-sm text-[var(--text-primary)] hover:bg-[var(--bg-hover)] flex items-center gap-2"><Save size={14} />Save</button>
            <button onClick={() => { setSaveName(fileName); setShowSaveDialog(true); }} className="w-full text-left px-3 py-1.5 text-sm text-[var(--text-primary)] hover:bg-[var(--bg-hover)] flex items-center gap-2"><SaveAll size={14} />Save As</button>
          </div>
        </div>
        <div className="relative group">
          <button className="px-2 py-1 rounded text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]">Edit</button>
          <div className="absolute top-full left-0 w-40 py-1 rounded-lg hidden group-hover:block z-50" style={{ background: 'var(--bg-panel)', boxShadow: 'var(--shadow-md)', border: '1px solid rgba(0,0,0,0.10)' }}>
            <button onClick={handleCut} className="w-full text-left px-3 py-1.5 text-sm text-[var(--text-primary)] hover:bg-[var(--bg-hover)] flex items-center gap-2"><Scissors size={14} />Cut</button>
            <button onClick={handleCopy} className="w-full text-left px-3 py-1.5 text-sm text-[var(--text-primary)] hover:bg-[var(--bg-hover)] flex items-center gap-2"><Copy size={14} />Copy</button>
            <button onClick={handlePaste} className="w-full text-left px-3 py-1.5 text-sm text-[var(--text-primary)] hover:bg-[var(--bg-hover)] flex items-center gap-2"><ClipboardPaste size={14} />Paste</button>
            <button onClick={handleSelectAll} className="w-full text-left px-3 py-1.5 text-sm text-[var(--text-primary)] hover:bg-[var(--bg-hover)]">Select All</button>
            <button onClick={() => setFindOpen(!findOpen)} className="w-full text-left px-3 py-1.5 text-sm text-[var(--text-primary)] hover:bg-[var(--bg-hover)] flex items-center gap-2"><Search size={14} />Find</button>
          </div>
        </div>
        <div className="relative group">
          <button className="px-2 py-1 rounded text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]">Format</button>
          <div className="absolute top-full left-0 w-40 py-1 rounded-lg hidden group-hover:block z-50" style={{ background: 'var(--bg-panel)', boxShadow: 'var(--shadow-md)', border: '1px solid rgba(0,0,0,0.10)' }}>
            <button onClick={() => setWordWrap(!wordWrap)} className="w-full text-left px-3 py-1.5 text-sm text-[var(--text-primary)] hover:bg-[var(--bg-hover)] flex items-center gap-2"><WrapText size={14} />Word Wrap</button>
            <div className="px-3 py-1 text-xs text-[var(--text-muted)]">Font Size: {fontSize}px</div>
            <div className="flex px-2 gap-1">
              <button onClick={() => setFontSize(Math.max(8, fontSize - 1))} className="px-2 py-0.5 rounded text-xs hover:bg-[var(--bg-hover)]">-</button>
              <button onClick={() => setFontSize(Math.min(32, fontSize + 1))} className="px-2 py-0.5 rounded text-xs hover:bg-[var(--bg-hover)]">+</button>
            </div>
          </div>
        </div>
        <div className="flex-1 text-xs text-[var(--text-muted)] truncate text-center">{fileName}{content !== (currentFileId ? readFile(currentFileId) : '') ? ' *' : ''}</div>
      </div>

      {/* Find/Replace bar */}
      {findOpen && (
        <div className="flex items-center gap-2 px-3 py-1.5 border-b" style={{ background: 'var(--bg-window)', borderColor: 'rgba(0,0,0,0.06)' }}>
          <input value={findText} onChange={(e) => setFindText(e.target.value)} placeholder="Find..." className="h-7 px-2 rounded text-xs outline-none flex-1" style={{ background: 'var(--bg-input)', color: 'var(--text-primary)', border: '1px solid rgba(0,0,0,0.06)' }} />
          <input value={replaceText} onChange={(e) => setReplaceText(e.target.value)} placeholder="Replace..." className="h-7 px-2 rounded text-xs outline-none flex-1" style={{ background: 'var(--bg-input)', color: 'var(--text-primary)', border: '1px solid rgba(0,0,0,0.06)' }} />
          <button onClick={handleFindReplace} className="px-3 py-1 rounded text-xs hover:bg-[var(--bg-hover)]">Replace All</button>
          <button onClick={() => setFindOpen(false)} className="text-xs text-[var(--text-muted)] hover:text-[var(--text-primary)]">Close</button>
        </div>
      )}

      {/* Editor area */}
      <div className="flex-1 flex overflow-hidden">
        {/* Line numbers */}
        <div className="w-12 py-2 text-right pr-2 select-none overflow-hidden" style={{ background: 'var(--bg-window)', color: 'var(--text-muted)', fontSize, fontFamily: "'JetBrains Mono', monospace", lineHeight: '1.6' }}>
          {lineNumbers.map((n) => <div key={n} className="leading-relaxed">{n}</div>)}
        </div>
        <textarea
          ref={textareaRef}
          value={content}
          onChange={(e) => setContent(e.target.value)}
          className="flex-1 p-2 outline-none resize-none font-mono leading-relaxed"
          style={{
            background: 'var(--bg-workspace)',
            color: 'var(--text-primary)',
            fontSize,
            whiteSpace: wordWrap ? 'pre-wrap' : 'pre',
            overflowWrap: wordWrap ? 'break-word' : 'normal',
            fontFamily: "'JetBrains Mono', monospace",
            lineHeight: '1.6',
          }}
          spellCheck={false}
        />
      </div>

      {/* Status bar */}
      <div className="flex items-center justify-between px-3 py-1 text-xs" style={{ background: 'var(--bg-window)', borderTop: '1px solid rgba(0,0,0,0.06)', color: 'var(--text-muted)' }}>
        <span>{lines} lines</span>
        <span>{chars} characters</span>
        <span>UTF-8</span>
        <span>{wordWrap ? 'Wrap' : 'No Wrap'}</span>
      </div>

      {/* Open dialog */}
      {showOpenDialog && (
        <div className="absolute inset-0 z-[60] flex items-center justify-center" style={{ background: 'rgba(0,0,0,0.5)' }}>
          <div className="w-96 rounded-xl p-4" style={{ background: 'var(--bg-panel)', boxShadow: 'var(--shadow-lg)' }}>
            <h3 className="text-sm font-semibold mb-3">Open File</h3>
            <div className="max-h-60 overflow-y-auto space-y-1">
              {getChildren(currentDirectory).filter((n) => n.type === 'file').map((node) => (
                <button key={node.id} onClick={() => handleOpen(node.id)} className="w-full text-left px-3 py-2 rounded hover:bg-[var(--bg-hover)] text-sm text-[var(--text-primary)]">
                  {node.name}
                </button>
              ))}
              {getChildren(currentDirectory).filter((n) => n.type === 'file').length === 0 && (
                <p className="text-sm text-[var(--text-muted)] text-center py-4">No files in current directory</p>
              )}
            </div>
            <button onClick={() => setShowOpenDialog(false)} className="w-full mt-3 py-2 rounded text-sm hover:bg-[var(--bg-hover)]" style={{ background: 'var(--bg-input)' }}>Cancel</button>
          </div>
        </div>
      )}

      {/* Save dialog */}
      {showSaveDialog && (
        <div className="absolute inset-0 z-[60] flex items-center justify-center" style={{ background: 'rgba(0,0,0,0.5)' }}>
          <div className="w-80 rounded-xl p-4" style={{ background: 'var(--bg-panel)', boxShadow: 'var(--shadow-lg)' }}>
            <h3 className="text-sm font-semibold mb-3">Save As</h3>
            <input value={saveName} onChange={(e) => setSaveName(e.target.value)} placeholder="Filename" autoFocus className="w-full h-9 px-3 rounded text-sm outline-none mb-3" style={{ background: 'var(--bg-input)', color: 'var(--text-primary)', border: '1px solid rgba(0,0,0,0.06)' }} />
            <div className="flex gap-2">
              <button onClick={() => setShowSaveDialog(false)} className="flex-1 py-2 rounded text-sm hover:bg-[var(--bg-hover)]" style={{ background: 'var(--bg-input)' }}>Cancel</button>
              <button onClick={handleSaveAs} className="flex-1 py-2 rounded text-sm text-white" style={{ background: 'var(--accent-dark-gray)' }}>Save</button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
