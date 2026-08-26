import { useState, useRef, useCallback } from 'react';
import { QRCodeCanvas } from 'qrcode.react';
import { Copy, Download, Link2, Settings } from 'lucide-react';

const EC_LEVELS = ['L', 'M', 'Q', 'H'] as const;
type ECLevel = typeof EC_LEVELS[number];

export default function QRCodeGenerator({ windowId: _windowId }: { windowId: string }) {
  const [text, setText] = useState('https://example.com');
  const [size, setSize] = useState(256);
  const [ecLevel, setEcLevel] = useState<ECLevel>('M');
  const [fgColor, setFgColor] = useState('#7D8B96');
  const [bgColor, setBgColor] = useState('#FFFFFF');
  const [copied, setCopied] = useState(false);
  const canvasRef = useRef<HTMLDivElement>(null);

  const download = useCallback(() => {
    const canvas = canvasRef.current?.querySelector('canvas');
    if (!canvas) return;
    const url = canvas.toDataURL('image/png');
    const a = document.createElement('a');
    a.href = url;
    a.download = 'qrcode.png';
    a.click();
  }, []);

  const copy = useCallback(async () => {
    const canvas = canvasRef.current?.querySelector('canvas');
    if (!canvas) return;
    canvas.toBlob(async (blob) => {
      if (!blob) return;
      await navigator.clipboard.write([new ClipboardItem({ 'image/png': blob })]);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  }, []);

  return (
    <div className="w-full h-full flex flex-col p-4 select-none overflow-auto" style={{ background: 'var(--bg-workspace)' }}>
      <div className="flex items-center gap-2 mb-4">
        <Link2 size={18} style={{ color: 'var(--accent-silver)' }} />
        <span className="text-sm font-medium" style={{ color: 'var(--text-primary)' }}>QR Code Generator</span>
      </div>

      {/* Input */}
      <div className="mb-3">
        <label className="text-xs mb-1 block" style={{ color: 'var(--text-muted)' }}>Text or URL</label>
        <textarea value={text} onChange={e => setText(e.target.value)}
          className="w-full p-2.5 rounded-lg text-sm outline-none resize-none font-mono"
          rows={3}
          style={{ background: 'var(--bg-input)', color: 'var(--text-primary)', border: '1px solid rgba(0,0,0,0.06)' }} />
      </div>

      {/* Options */}
      <div className="flex flex-wrap items-center gap-3 mb-4">
        <div className="flex items-center gap-2">
          <Settings size={12} style={{ color: 'var(--text-muted)' }} />
          <label className="text-xs" style={{ color: 'var(--text-muted)' }}>Size</label>
          <select value={size} onChange={e => setSize(parseInt(e.target.value))}
            className="text-xs rounded px-2 py-1 outline-none"
            style={{ background: 'var(--bg-input)', color: 'var(--text-primary)', border: '1px solid rgba(0,0,0,0.06)' }}>
            {[128, 192, 256, 320, 384, 512].map(s => <option key={s} value={s}>{s}px</option>)}
          </select>
        </div>
        <div className="flex items-center gap-2">
          <label className="text-xs" style={{ color: 'var(--text-muted)' }}>EC Level</label>
          <select value={ecLevel} onChange={e => setEcLevel(e.target.value as ECLevel)}
            className="text-xs rounded px-2 py-1 outline-none"
            style={{ background: 'var(--bg-input)', color: 'var(--text-primary)', border: '1px solid rgba(0,0,0,0.06)' }}>
            {EC_LEVELS.map(l => <option key={l} value={l}>{l}</option>)}
          </select>
        </div>
        <div className="flex items-center gap-2">
          <label className="text-xs" style={{ color: 'var(--text-muted)' }}>FG</label>
          <input type="color" value={fgColor} onChange={e => setFgColor(e.target.value)} className="w-6 h-6 rounded cursor-pointer border-0" />
          <label className="text-xs ml-1" style={{ color: 'var(--text-muted)' }}>BG</label>
          <input type="color" value={bgColor} onChange={e => setBgColor(e.target.value)} className="w-6 h-6 rounded cursor-pointer border-0" />
        </div>
      </div>

      {/* QR Display */}
      <div className="flex-1 flex flex-col items-center justify-center min-h-0">
        <div ref={canvasRef} className="p-4 rounded-xl mb-3" style={{ background: 'var(--bg-window)' }}>
          <QRCodeCanvas value={text || ' '} size={size} level={ecLevel} fgColor={fgColor} bgColor={bgColor} includeMargin={true} />
        </div>
        <div className="flex gap-2">
          <button onClick={download} className="px-3 py-1.5 rounded-lg text-xs font-medium flex items-center gap-1 text-white" style={{ background: 'var(--accent-dark-gray)' }}>
            <Download size={12} /> Download PNG
          </button>
          <button onClick={copy} className="px-3 py-1.5 rounded-lg text-xs font-medium flex items-center gap-1" style={{ background: 'var(--bg-input)', color: 'var(--accent-silver)' }}>
            <Copy size={12} /> {copied ? 'Copied!' : 'Copy'}
          </button>
        </div>
      </div>
    </div>
  );
}
