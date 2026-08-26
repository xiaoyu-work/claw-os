import { useState, useCallback, useEffect } from 'react';
import { Delete, Equal, Plus, Minus, X, Divide, Percent, Radical, RotateCcw, ChevronLeft } from 'lucide-react';

interface CalculatorProps {
  windowId: string;
}

export default function Calculator({ windowId }: CalculatorProps) {
  const [display, setDisplay] = useState('0');
  const [prev, setPrev] = useState<string | null>(null);
  const [op, setOp] = useState<string | null>(null);
  const [scientific, setScientific] = useState(false);
  const [history, setHistory] = useState<string[]>([]);
  const [newEntry, setNewEntry] = useState(true);

  const clear = () => { setDisplay('0'); setPrev(null); setOp(null); };
  const inputDigit = (d: string) => {
    if (newEntry) { setDisplay(d); setNewEntry(false); }
    else { setDisplay(display === '0' ? d : display + d); }
  };
  const inputDecimal = () => {
    if (newEntry) { setDisplay('0.'); setNewEntry(false); }
    else if (!display.includes('.')) { setDisplay(display + '.'); }
  };

  const calculate = useCallback((a: number, b: number, operation: string): number => {
    switch (operation) {
      case '+': return a + b;
      case '-': return a - b;
      case '*': return a * b;
      case '/': return b === 0 ? NaN : a / b;
      case '%': return a * (b / 100);
      case '^': return Math.pow(a, b);
      default: return b;
    }
  }, []);

  const handleOp = (operation: string) => {
    if (prev !== null && op && !newEntry) {
      const result = calculate(parseFloat(prev), parseFloat(display), op);
      const resStr = Number.isFinite(result) ? String(result).slice(0, 12) : 'Error';
      setHistory((h) => [`${prev} ${op} ${display} = ${resStr}`, ...h].slice(0, 20));
      setPrev(resStr);
      setDisplay(resStr);
    } else {
      setPrev(display);
    }
    setOp(operation);
    setNewEntry(true);
  };

  const handleEquals = () => {
    if (prev === null || op === null) return;
    const result = calculate(parseFloat(prev), parseFloat(display), op);
    const resStr = Number.isFinite(result) ? String(result).slice(0, 12) : 'Error';
    setHistory((h) => [`${prev} ${op} ${display} = ${resStr}`, ...h].slice(0, 20));
    setDisplay(resStr);
    setPrev(null);
    setOp(null);
    setNewEntry(true);
  };

  const handleScientific = (fn: string) => {
    const v = parseFloat(display);
    let result = 0;
    switch (fn) {
      case 'sin': result = Math.sin(v); break;
      case 'cos': result = Math.cos(v); break;
      case 'tan': result = Math.tan(v); break;
      case 'log': result = Math.log10(v); break;
      case 'ln': result = Math.log(v); break;
      case 'sqrt': result = Math.sqrt(v); break;
      case 'exp': result = Math.exp(v); break;
      case 'factorial': result = v <= 0 ? 1 : Array.from({ length: Math.floor(v) }, (_, i) => i + 1).reduce((a, b) => a * b, 1); break;
    }
    const resStr = Number.isFinite(result) ? String(result).slice(0, 12) : 'Error';
    setHistory((h) => [`${fn}(${display}) = ${resStr}`, ...h].slice(0, 20));
    setDisplay(resStr);
    setNewEntry(true);
  };

  const handlePercent = () => handleOp('%');
  const handleNegate = () => setDisplay(String(parseFloat(display) * -1));
  const handleBackspace = () => { setDisplay(display.length > 1 ? display.slice(0, -1) : '0'); };

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key >= '0' && e.key <= '9') inputDigit(e.key);
      if (e.key === '.') inputDecimal();
      if (e.key === '+') handleOp('+');
      if (e.key === '-') handleOp('-');
      if (e.key === '*') handleOp('*');
      if (e.key === '/') { e.preventDefault(); handleOp('/'); }
      if (e.key === 'Enter' || e.key === '=') handleEquals();
      if (e.key === 'Backspace') handleBackspace();
      if (e.key === 'Escape') clear();
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [display, prev, op, newEntry]);

  const btnBase = 'h-12 rounded-lg text-sm font-medium transition-all active:scale-95 flex items-center justify-center';
  const btnNum = `${btnBase} hover:bg-[var(--bg-hover)] text-[var(--text-primary)]`;
  const btnOp = `${btnBase} text-[var(--accent-silver)]`;
  const btnEq = `${btnBase} text-white`;
  const btnSci = `${btnBase} text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]`;

  return (
    <div className="w-full h-full flex flex-col p-3" style={{ background: 'var(--bg-workspace)' }}>
      {/* Display */}
      <div className="mb-2 px-3 py-2 rounded-lg text-right" style={{ background: 'var(--bg-window)' }}>
        <div className="text-xs text-[var(--text-muted)] h-4">{prev || ''} {op || ''}</div>
        <div className="text-2xl font-mono text-[var(--text-primary)] truncate">{display}</div>
      </div>

      {/* History */}
      <div className="flex-1 overflow-y-auto mb-2 px-2" style={{ maxHeight: scientific ? 60 : 100 }}>
        {history.map((h, i) => (
          <div key={i} className="text-xs text-[var(--text-muted)] text-right py-0.5">{h}</div>
        ))}
      </div>

      {/* Mode toggle */}
      <button onClick={() => setScientific(!scientific)} className="text-xs text-[var(--accent-silver)] mb-1 hover:underline self-end">
        {scientific ? 'Standard' : 'Scientific'}
      </button>

      {/* Keypad */}
      <div className="grid gap-1.5" style={{ gridTemplateColumns: scientific ? 'repeat(5, 1fr)' : 'repeat(4, 1fr)' }}>
        {scientific && (
          <>
            <button onClick={() => handleScientific('sin')} className={btnSci}>sin</button>
            <button onClick={() => handleScientific('cos')} className={btnSci}>cos</button>
            <button onClick={() => handleScientific('tan')} className={btnSci}>tan</button>
            <button onClick={() => handleScientific('log')} className={btnSci}>log</button>
            <button onClick={() => handleScientific('ln')} className={btnSci}>ln</button>
            <button onClick={() => handleScientific('sqrt')} className={btnSci}>√</button>
            <button onClick={() => handleScientific('exp')} className={btnSci}>exp</button>
            <button onClick={() => handleOp('^')} className={btnSci}>x^y</button>
            <button onClick={() => handleScientific('factorial')} className={btnSci}>n!</button>
            <button onClick={() => inputDigit(String(Math.PI).slice(0, 10))} className={btnSci}>π</button>
          </>
        )}
        <button onClick={clear} className={`${btnOp}`} style={{ background: 'var(--bg-input)' }}><RotateCcw size={16} /></button>
        <button onClick={handleBackspace} className={btnOp} style={{ background: 'var(--bg-input)' }}><Delete size={16} /></button>
        <button onClick={handlePercent} className={btnOp} style={{ background: 'var(--bg-input)' }}><Percent size={16} /></button>
        <button onClick={handleNegate} className={btnOp} style={{ background: 'var(--bg-input)' }}>+/-</button>
        {!scientific && <button onClick={() => handleOp('/')} className={btnOp} style={{ background: 'var(--bg-input)' }}><Divide size={16} /></button>}
        {scientific && <button onClick={() => handleOp('/')} className={btnOp} style={{ background: 'var(--bg-input)' }}><Divide size={16} /></button>}

        {['7', '8', '9'].map((d) => <button key={d} onClick={() => inputDigit(d)} className={btnNum} style={{ background: 'var(--bg-input)' }}>{d}</button>)}
        <button onClick={() => handleOp('*')} className={btnOp} style={{ background: 'var(--bg-input)' }}><X size={16} /></button>
        {scientific && <button onClick={() => inputDigit(String(Math.E).slice(0, 10))} className={btnSci}>e</button>}

        {['4', '5', '6'].map((d) => <button key={d} onClick={() => inputDigit(d)} className={btnNum} style={{ background: 'var(--bg-input)' }}>{d}</button>)}
        <button onClick={() => handleOp('-')} className={btnOp} style={{ background: 'var(--bg-input)' }}><Minus size={16} /></button>
        {scientific && <button onClick={() => { setDisplay(String(1 / parseFloat(display))); setNewEntry(true); }} className={btnSci}>1/x</button>}

        {['1', '2', '3'].map((d) => <button key={d} onClick={() => inputDigit(d)} className={btnNum} style={{ background: 'var(--bg-input)' }}>{d}</button>)}
        <button onClick={() => handleOp('+')} className={btnOp} style={{ background: 'var(--bg-input)' }}><Plus size={16} /></button>
        {scientific && <button onClick={() => { setDisplay(String(parseFloat(display) ** 2)); setNewEntry(true); }} className={btnSci}>x²</button>}

        <button onClick={() => inputDigit('0')} className={`${btnNum} col-span-2`} style={{ background: 'var(--bg-input)' }}>0</button>
        <button onClick={inputDecimal} className={btnNum} style={{ background: 'var(--bg-input)' }}>.</button>
        <button onClick={handleEquals} className={btnEq} style={{ background: 'var(--accent-dark-gray)' }}><Equal size={16} /></button>
        {scientific && <div />}
      </div>
    </div>
  );
}
