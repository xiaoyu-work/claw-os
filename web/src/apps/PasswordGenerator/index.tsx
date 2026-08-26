import { useState, useCallback, useEffect } from 'react';
import { Copy, RotateCcw, Shield, ShieldCheck, ShieldAlert, History } from 'lucide-react';

const UPPER = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ';
const LOWER = 'abcdefghijklmnopqrstuvwxyz';
const NUMBERS = '0123456789';
const SYMBOLS = '!@#$%^&*()_+-=[]{}|;:,.<>?';

function generate(length: number, useUpper: boolean, useLower: boolean, useNumbers: boolean, useSymbols: boolean): string {
  let chars = '';
  if (useUpper) chars += UPPER;
  if (useLower) chars += LOWER;
  if (useNumbers) chars += NUMBERS;
  if (useSymbols) chars += SYMBOLS;
  if (chars === '') return '';
  let result = '';
  const arr = new Uint32Array(length);
  crypto.getRandomValues(arr);
  for (let i = 0; i < length; i++) result += chars[arr[i] % chars.length];
  return result;
}

function getStrength(password: string): { label: string; color: string; percent: number; icon: typeof Shield } {
  let score = 0;
  if (password.length >= 8) score++;
  if (password.length >= 12) score++;
  if (password.length >= 16) score++;
  if (/[A-Z]/.test(password)) score++;
  if (/[a-z]/.test(password)) score++;
  if (/[0-9]/.test(password)) score++;
  if (/[^A-Za-z0-9]/.test(password)) score++;
  if (score <= 2) return { label: 'Weak', color: 'var(--error)', percent: 25, icon: ShieldAlert };
  if (score <= 4) return { label: 'Fair', color: 'var(--warning)', percent: 50, icon: Shield };
  if (score <= 6) return { label: 'Good', color: '#5A7A8A', percent: 75, icon: ShieldCheck };
  return { label: 'Strong', color: 'var(--success)', percent: 100, icon: ShieldCheck };
}

export default function PasswordGenerator({ windowId: _windowId }: { windowId: string }) {
  const [length, setLength] = useState(16);
  const [useUpper, setUseUpper] = useState(true);
  const [useLower, setUseLower] = useState(true);
  const [useNumbers, setUseNumbers] = useState(true);
  const [useSymbols, setUseSymbols] = useState(true);
  const [password, setPassword] = useState('');
  const [history, setHistory] = useState<string[]>(() => JSON.parse(localStorage.getItem('pw_history') || '[]'));
  const [copied, setCopied] = useState(false);

  const regenerate = useCallback(() => {
    const pw = generate(length, useUpper, useLower, useNumbers, useSymbols);
    setPassword(pw);
  }, [length, useUpper, useLower, useNumbers, useSymbols]);

  useEffect(() => { regenerate(); }, [regenerate]);

  const copy = async () => {
    if (!password) return;
    await navigator.clipboard.writeText(password);
    setCopied(true);
    setHistory(prev => {
      const next = [password, ...prev].slice(0, 10);
      localStorage.setItem('pw_history', JSON.stringify(next));
      return next;
    });
    setTimeout(() => setCopied(false), 1500);
  };

  const strength = getStrength(password);
  const StrengthIcon = strength.icon;

  return (
    <div className="w-full h-full flex flex-col p-4 select-none overflow-auto" style={{ background: 'var(--bg-workspace)' }}>
      <div className="flex items-center gap-2 mb-4">
        <StrengthIcon size={18} style={{ color: strength.color }} />
        <span className="text-sm font-medium" style={{ color: 'var(--text-primary)' }}>Password Generator</span>
      </div>

      {/* Password display */}
      <div className="p-3 rounded-xl mb-3 flex items-center gap-2" style={{ background: 'var(--bg-window)' }}>
        <input value={password} readOnly className="flex-1 bg-transparent text-sm font-mono outline-none" style={{ color: 'var(--text-primary)' }} />
        <button onClick={copy} className="p-1.5 rounded-lg hover:bg-[var(--bg-hover)] transition-all" title="Copy" style={{ color: copied ? 'var(--success)' : 'var(--accent-silver)' }}>
          <Copy size={14} />
        </button>
        <button onClick={regenerate} className="p-1.5 rounded-lg hover:bg-[var(--bg-hover)] transition-all" title="Generate" style={{ color: 'var(--accent-silver)' }}>
          <RotateCcw size={14} />
        </button>
      </div>

      {/* Strength meter */}
      <div className="mb-4">
        <div className="flex items-center justify-between mb-1">
          <span className="text-xs" style={{ color: 'var(--text-muted)' }}>Strength</span>
          <span className="text-xs font-medium" style={{ color: strength.color }}>{strength.label}</span>
        </div>
        <div className="h-1.5 rounded-full overflow-hidden" style={{ background: 'var(--bg-input)' }}>
          <div className="h-full rounded-full transition-all" style={{ width: `${strength.percent}%`, background: strength.color }} />
        </div>
      </div>

      {/* Length slider */}
      <div className="mb-3">
        <div className="flex items-center justify-between mb-1">
          <span className="text-xs" style={{ color: 'var(--text-muted)' }}>Length</span>
          <span className="text-xs font-medium" style={{ color: 'var(--accent-silver)' }}>{length}</span>
        </div>
        <input type="range" min={8} max={64} value={length} onChange={e => setLength(parseInt(e.target.value))}
          className="w-full h-1 rounded-full appearance-none cursor-pointer" style={{ accentColor: 'var(--accent-silver)' }} />
      </div>

      {/* Toggles */}
      <div className="grid grid-cols-2 gap-2 mb-4">
        {[
          { label: 'Uppercase', state: useUpper, set: setUseUpper },
          { label: 'Lowercase', state: useLower, set: setUseLower },
          { label: 'Numbers', state: useNumbers, set: setUseNumbers },
          { label: 'Symbols', state: useSymbols, set: setUseSymbols },
        ].map(t => (
          <button key={t.label} onClick={() => t.set(!t.state)}
            className="px-3 py-2 rounded-lg text-xs font-medium flex items-center justify-between transition-all"
            style={{ background: t.state ? 'rgba(125,139,150,0.15)' : 'var(--bg-input)', color: t.state ? 'var(--accent-silver)' : 'var(--text-muted)', border: t.state ? '1px solid rgba(125,139,150,0.3)' : '1px solid transparent' }}>
            {t.label}
            <div className="w-8 h-4 rounded-full relative transition-all" style={{ background: t.state ? 'var(--accent-dark-gray)' : 'var(--bg-hover)' }}>
              <div className="w-3 h-3 rounded-full absolute top-0.5 transition-all" style={{ background: '#fff', left: t.state ? 18 : 2 }} />
            </div>
          </button>
        ))}
      </div>

      {/* History */}
      <div className="flex-1 overflow-hidden flex flex-col">
        <div className="flex items-center gap-1 mb-1 text-xs" style={{ color: 'var(--text-muted)' }}><History size={10} /> History</div>
        <div className="flex-1 overflow-y-auto">
          {history.length === 0 && <div className="text-xs italic" style={{ color: 'var(--text-muted)' }}>No history yet</div>}
          {history.map((h, i) => (
            <div key={i} className="flex items-center gap-2 py-1 text-xs font-mono" style={{ color: 'var(--text-secondary)' }}>
              <span className="truncate flex-1">{h}</span>
              <button onClick={() => navigator.clipboard.writeText(h)} className="p-0.5 rounded hover:bg-[var(--bg-hover)] shrink-0" style={{ color: 'var(--text-muted)' }}><Copy size={10} /></button>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
