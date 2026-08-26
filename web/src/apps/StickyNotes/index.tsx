import { useState, useEffect, useCallback } from 'react';
import { Plus, Trash2, Palette } from 'lucide-react';

const NOTE_COLORS = [
  { name: 'yellow', bg: '#FEF3C7', text: '#92400E' },
  { name: 'green', bg: '#D1FAE5', text: '#065F46' },
  { name: 'pink', bg: '#FCE7F3', text: '#9D174D' },
  { name: 'blue', bg: '#DBEAFE', text: '#1E40AF' },
  { name: 'purple', bg: '#EDE9FE', text: '#5B21B6' },
];

interface Note { id: string; title: string; content: string; color: string; createdAt: number; updatedAt: number }

function loadNotes(): Note[] {
  try { return JSON.parse(localStorage.getItem('sticky_notes') || '[]'); } catch { return []; }
}
function saveNotes(notes: Note[]) { localStorage.setItem('sticky_notes', JSON.stringify(notes)); }

export default function StickyNotes({ windowId: _windowId }: { windowId: string }) {
  const [notes, setNotes] = useState<Note[]>(loadNotes);
  const [selectedId, setSelectedId] = useState<string | null>(null);

  useEffect(() => { saveNotes(notes); }, [notes]);

  const addNote = useCallback(() => {
    const newNote: Note = { id: Date.now().toString(), title: 'New Note', content: '', color: NOTE_COLORS[0].name, createdAt: Date.now(), updatedAt: Date.now() };
    setNotes(prev => [newNote, ...prev]);
    setSelectedId(newNote.id);
  }, []);

  const updateNote = useCallback((id: string, updates: Partial<Note>) => {
    setNotes(prev => prev.map(n => n.id === id ? { ...n, ...updates, updatedAt: Date.now() } : n));
  }, []);

  const deleteNote = useCallback((id: string) => {
    setNotes(prev => prev.filter(n => n.id !== id));
    if (selectedId === id) setSelectedId(null);
  }, [selectedId]);

  const selected = notes.find(n => n.id === selectedId);

  const formatDate = (ts: number) => new Date(ts).toLocaleDateString('en-US', { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });

  return (
    <div className="w-full h-full flex" style={{ background: 'var(--bg-workspace)' }}>
      {/* Sidebar */}
      <div className="w-48 flex flex-col border-r" style={{ borderColor: 'rgba(0,0,0,0.06)', background: 'var(--bg-workspace)' }}>
        <div className="flex items-center justify-between p-2 border-b" style={{ borderColor: 'rgba(0,0,0,0.06)' }}>
          <span className="text-xs font-medium" style={{ color: 'var(--text-secondary)' }}>Notes ({notes.length})</span>
          <button onClick={addNote} className="p-1 rounded hover:bg-[var(--bg-hover)]" style={{ color: 'var(--accent-silver)' }}><Plus size={14} /></button>
        </div>
        <div className="flex-1 overflow-y-auto">
          {notes.length === 0 && (
            <div className="p-4 text-center text-xs italic" style={{ color: 'var(--text-muted)' }}>No notes yet</div>
          )}
          {notes.map(note => {
            const color = NOTE_COLORS.find(c => c.name === note.color) || NOTE_COLORS[0];
            return (
              <button key={note.id} onClick={() => setSelectedId(note.id)}
                className="w-full text-left p-2 border-b transition-all hover:bg-[var(--bg-hover)]"
                style={{ borderColor: 'rgba(0,0,0,0.04)', background: selectedId === note.id ? 'rgba(125,139,150,0.1)' : 'transparent' }}>
                <div className="flex items-center gap-2">
                  <div className="w-3 h-3 rounded-full shrink-0" style={{ background: color.bg }} />
                  <span className="text-xs font-medium truncate flex-1" style={{ color: 'var(--text-primary)' }}>{note.title || 'Untitled'}</span>
                </div>
                <div className="text-[10px] mt-0.5 truncate ml-5" style={{ color: 'var(--text-muted)' }}>{note.content || 'No content'}</div>
                <div className="text-[9px] mt-0.5 ml-5" style={{ color: 'var(--text-muted)' }}>{formatDate(note.updatedAt)}</div>
              </button>
            );
          })}
        </div>
      </div>

      {/* Editor */}
      <div className="flex-1 flex flex-col">
        {selected ? (
          <>
            <div className="flex items-center justify-between p-2 border-b" style={{ borderColor: 'rgba(0,0,0,0.06)' }}>
              <div className="flex items-center gap-2">
                <Palette size={12} style={{ color: 'var(--text-muted)' }} />
                {NOTE_COLORS.map(c => (
                  <button key={c.name} onClick={() => updateNote(selected.id, { color: c.name })}
                    className="w-4 h-4 rounded-full transition-transform hover:scale-110"
                    style={{ background: c.bg, border: selected.color === c.name ? '2px solid var(--accent-silver)' : '1px solid rgba(0,0,0,0.12)' }} />
                ))}
              </div>
              <button onClick={() => deleteNote(selected.id)} className="p-1 rounded hover:bg-[var(--bg-hover)]" style={{ color: 'var(--error)' }}><Trash2 size={14} /></button>
            </div>
            <input value={selected.title} onChange={e => updateNote(selected.id, { title: e.target.value })}
              className="w-full px-3 py-2 text-sm font-medium bg-transparent outline-none"
              style={{ color: 'var(--text-primary)', borderBottom: '1px solid rgba(0,0,0,0.06)' }} placeholder="Note title..." />
            <textarea value={selected.content} onChange={e => updateNote(selected.id, { content: e.target.value })}
              className="flex-1 w-full px-3 py-2 text-sm bg-transparent outline-none resize-none"
              style={{ color: 'var(--text-primary)' }} placeholder="Write your note here..." />
            <div className="px-3 py-1 text-[10px] border-t" style={{ color: 'var(--text-muted)', borderColor: 'rgba(0,0,0,0.06)' }}>
              Created {formatDate(selected.createdAt)} | Updated {formatDate(selected.updatedAt)}
            </div>
          </>
        ) : (
          <div className="flex-1 flex flex-col items-center justify-center">
            <div className="w-12 h-12 rounded-2xl flex items-center justify-center mb-3" style={{ background: 'var(--bg-input)' }}>
              <span className="text-2xl" style={{ color: 'var(--accent-silver)' }}>+</span>
            </div>
            <p className="text-xs" style={{ color: 'var(--text-muted)' }}>Select a note or create a new one</p>
            <button onClick={addNote} className="mt-3 px-4 py-2 rounded-lg text-xs font-medium text-white" style={{ background: 'var(--accent-dark-gray)' }}>New Note</button>
          </div>
        )}
      </div>
    </div>
  );
}
