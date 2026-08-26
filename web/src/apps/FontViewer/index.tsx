import { useState } from 'react';
import { Type, Bold, Italic, Underline, Palette } from 'lucide-react';

const FONTS = [
  'Inter', 'Arial', 'Georgia', 'Times New Roman', 'Courier New', 'Verdana',
  'Tahoma', 'Trebuchet MS', 'Palatino Linotype', 'Garamond',
  'Comic Sans MS', 'Impact', 'Lucida Console', 'Consolas',
  'Segoe UI', 'Helvetica', 'Futura', 'Baskerville', 'Cambria', 'Didot',
];

export default function FontViewer({ windowId: _windowId }: { windowId: string }) {
  const [text, setText] = useState('The quick brown fox jumps over the lazy dog.\n\nABCDEFGHIJKLMNOPQRSTUVWXYZ\nabcdefghijklmnopqrstuvwxyz\n0123456789');
  const [fontFamily, setFontFamily] = useState('Inter');
  const [fontSize, setFontSize] = useState(24);
  const [isBold, setIsBold] = useState(false);
  const [isItalic, setIsItalic] = useState(false);
  const [isUnderline, setIsUnderline] = useState(false);
  const [color, setColor] = useState('#E2E8F0');
  const [showColorPicker, setShowColorPicker] = useState(false);

  const style: React.CSSProperties = {
    fontFamily,
    fontSize: `${fontSize}px`,
    fontWeight: isBold ? 'bold' : 'normal',
    fontStyle: isItalic ? 'italic' : 'normal',
    textDecoration: isUnderline ? 'underline' : 'none',
    color,
    lineHeight: 1.5,
  };

  return (
    <div className="w-full h-full flex flex-col" style={{ background: 'var(--bg-workspace)' }}>
      {/* Toolbar */}
      <div className="flex flex-wrap items-center gap-2 p-2 border-b" style={{ borderColor: 'rgba(0,0,0,0.06)', background: 'var(--bg-workspace)' }}>
        <Type size={14} style={{ color: 'var(--accent-silver)' }} />

        {/* Font select */}
        <select value={fontFamily} onChange={e => setFontFamily(e.target.value)}
          className="text-xs rounded px-2 py-1 outline-none min-w-[120px]"
          style={{ background: 'var(--bg-input)', color: 'var(--text-primary)', border: '1px solid rgba(0,0,0,0.06)' }}>
          {FONTS.map(f => <option key={f} value={f}>{f}</option>)}
        </select>

        {/* Size */}
        <div className="flex items-center gap-1">
          <span className="text-[10px]" style={{ color: 'var(--text-muted)' }}>{fontSize}px</span>
          <input type="range" min={12} max={72} value={fontSize} onChange={e => setFontSize(parseInt(e.target.value))}
            className="w-20 h-1 rounded-full appearance-none" style={{ accentColor: 'var(--accent-silver)' }} />
        </div>

        {/* Format toggles */}
        <button onClick={() => setIsBold(!isBold)} className="p-1 rounded transition-all" style={{ background: isBold ? 'rgba(125,139,150,0.2)' : 'transparent', color: isBold ? 'var(--accent-silver)' : 'var(--text-muted)' }}><Bold size={14} /></button>
        <button onClick={() => setIsItalic(!isItalic)} className="p-1 rounded transition-all" style={{ background: isItalic ? 'rgba(125,139,150,0.2)' : 'transparent', color: isItalic ? 'var(--accent-silver)' : 'var(--text-muted)' }}><Italic size={14} /></button>
        <button onClick={() => setIsUnderline(!isUnderline)} className="p-1 rounded transition-all" style={{ background: isUnderline ? 'rgba(125,139,150,0.2)' : 'transparent', color: isUnderline ? 'var(--accent-silver)' : 'var(--text-muted)' }}><Underline size={14} /></button>

        {/* Color */}
        <div className="relative">
          <button onClick={() => setShowColorPicker(!showColorPicker)} className="p-1 rounded" style={{ color: 'var(--text-muted)' }}><Palette size={14} /></button>
          {showColorPicker && (
            <div className="absolute top-full left-0 mt-1 p-2 rounded-lg z-10 grid grid-cols-5 gap-1" style={{ background: 'var(--bg-window)', border: '1px solid rgba(0,0,0,0.06)' }}>
              {['#E2E8F0', '#EF4444', '#F59E0B', '#22C55E', '#5A7A8A', '#7D8B96', '#EC4899', '#06B6D4', '#F97316', '#84CC16'].map(c => (
                <button key={c} onClick={() => { setColor(c); setShowColorPicker(false); }} className="w-5 h-5 rounded-full" style={{ background: c }} />
              ))}
            </div>
          )}
        </div>

        <div className="ml-auto text-xs" style={{ color: 'var(--text-muted)' }}>{fontFamily}</div>
      </div>

      {/* Preview */}
      <div className="flex-1 p-4 overflow-auto">
        <textarea value={text} onChange={e => setText(e.target.value)}
          className="w-full h-full bg-transparent outline-none resize-none"
          style={{ ...style, background: 'transparent' }} />
      </div>

      {/* Font samples */}
      <div className="h-24 border-t overflow-auto p-2" style={{ borderColor: 'rgba(0,0,0,0.06)', background: 'var(--bg-window)' }}>
        <div className="text-[10px] mb-1" style={{ color: 'var(--text-muted)' }}>Quick Preview</div>
        <div className="flex flex-col gap-1">
          {FONTS.slice(0, 10).map(f => (
            <button key={f} onClick={() => setFontFamily(f)}
              className="text-left text-xs py-0.5 px-1 rounded hover:bg-[var(--bg-hover)] transition-all truncate"
              style={{ color: 'var(--text-secondary)', fontFamily: f }}>
              {f} - Aa Bb Cc 123
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
