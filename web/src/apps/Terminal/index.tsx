import { useState, useRef, useEffect, useCallback } from 'react';
import { executeCommand, getCompletions } from './commands';
import { useFileSystemStore } from '@/stores/useFileSystemStore';

interface TerminalProps {
  windowId: string;
}

export default function Terminal({ windowId }: TerminalProps) {
  const [lines, setLines] = useState<string[]>([
    'WebOS 24.04 LTS \x1b[1;36mwebos-desktop\x1b[0m tty1',
    '',
    'Welcome to WebOS Terminal!',
    'Type \x1b[1;35mhelp\x1b[0m for available commands.',
    '',
  ]);
  const [input, setInput] = useState('');
  const [history, setHistory] = useState<string[]>([]);
  const [historyIndex, setHistoryIndex] = useState(-1);
  const [cwd, setCwd] = useState('fs-user');
  const [completions, setCompletions] = useState<string[]>([]);
  const [compIndex, setCompIndex] = useState(-1);
  const inputRef = useRef<HTMLInputElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (scrollRef.current) scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
  }, [lines]);

  const getPrompt = () => {
    const fs = useFileSystemStore.getState();
    const path = fs.getPath(cwd);
    return `\x1b[1;32muser@webos\x1b[0m:\n\x1b[1;34m${path}\x1b[0m$ `;
  };

  const handleSubmit = useCallback(() => {
    if (!input.trim()) {
      setLines((prev) => [...prev, getPrompt()]);
      return;
    }

    const trimmed = input.trim();
    setHistory((prev) => [...prev, trimmed]);
    setHistoryIndex(-1);

    const result = executeCommand(trimmed, {
      cwd,
      setCwd,
      history: [...history, trimmed],
      addToHistory: (cmd) => setHistory((p) => [...p, cmd]),
    });

    if (result === '__CLEAR__') {
      setLines([]);
    } else if (result === '__EXIT__') {
      // Do nothing, the terminal will be closed by the window manager
    } else {
      const prompt = getPrompt();
      if (result) {
        setLines((prev) => [...prev, prompt + trimmed, result]);
      } else {
        setLines((prev) => [...prev, prompt + trimmed]);
      }
    }

    setInput('');
    setCompletions([]);
    setCompIndex(-1);
  }, [input, cwd, history]);

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter') { handleSubmit(); return; }
    if (e.key === 'ArrowUp') {
      e.preventDefault();
      if (history.length > 0) {
        const newIdx = historyIndex < 0 ? history.length - 1 : Math.max(0, historyIndex - 1);
        setHistoryIndex(newIdx);
        setInput(history[newIdx] || '');
      }
      return;
    }
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      if (historyIndex >= 0) {
        const newIdx = historyIndex + 1;
        if (newIdx >= history.length) { setHistoryIndex(-1); setInput(''); }
        else { setHistoryIndex(newIdx); setInput(history[newIdx]); }
      }
      return;
    }
    if (e.key === 'Tab') {
      e.preventDefault();
      if (completions.length > 0 && compIndex >= 0) {
        const nextIdx = (compIndex + 1) % completions.length;
        setCompIndex(nextIdx);
        setInput(completions[nextIdx] + ' ');
      } else {
        const comps = getCompletions(input);
        if (comps.length > 0) {
          setCompletions(comps);
          setCompIndex(0);
          setInput(comps[0] + ' ');
        }
      }
      return;
    }
    if (e.key !== 'Tab') { setCompletions([]); setCompIndex(-1); }
  };

  // ANSI color parser for rendering
  const renderAnsi = (text: string) => {
    const parts = text.split(/(\x1b\[[0-9;]*m)/g);
    const elements: React.ReactNode[] = [];
    let color = '#E8E8E8';
    let bold = false;
    let spanIdx = 0;

    for (const part of parts) {
      if (part.startsWith('\x1b[')) {
        const codes = part.slice(2, -1).split(';').map(Number);
        for (const code of codes) {
          if (code === 0) { color = '#E8E8E8'; bold = false; }
          if (code === 1) bold = true;
          if (code === 35) color = '#7D8B96';
          if (code === 36) color = '#22D3EE';
          if (code === 32) color = '#22C55E';
          if (code === 34) color = '#5A7A8A';
          if (code === 31) color = '#EF4444';
          if (code === 33) color = '#F59E0B';
        }
        continue;
      }
      if (part) {
        elements.push(
          <span key={spanIdx++} style={{ color, fontWeight: bold ? 600 : 400 }}>{part}</span>
        );
      }
    }
    return elements;
  };

  return (
    <div className="w-full h-full flex flex-col font-mono text-sm" style={{ background: '#0D0D1A', color: '#E8E8E8' }}>
      <div ref={scrollRef} className="flex-1 overflow-y-auto p-3 pb-0">
        {lines.map((line, i) => (
          <div key={i} className="whitespace-pre-wrap break-all leading-relaxed">
            {line.includes('\x1b[') ? renderAnsi(line) : line}
          </div>
        ))}
        {/* Current input line */}
        <div className="flex items-start whitespace-pre-wrap break-all leading-relaxed min-h-[24px]">
          <span>{renderAnsi(getPrompt())}</span>
          <input
            ref={inputRef}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            className="flex-1 bg-transparent outline-none font-mono text-sm min-w-[100px]"
            style={{ color: '#E8E8E8', caretColor: '#A78BFA' }}
            autoFocus
            spellCheck={false}
            autoComplete="off"
            autoCorrect="off"
          />
        </div>
      </div>
    </div>
  );
}
