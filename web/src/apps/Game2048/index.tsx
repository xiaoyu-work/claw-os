import { useState, useEffect, useCallback } from 'react';
import { RotateCcw, Trophy } from 'lucide-react';

const GRID = 4;

type Board = (number | null)[][];

function createEmpty(): Board {
  return Array.from({ length: GRID }, () => Array(GRID).fill(null));
}

function addRandom(board: Board): Board {
  const empty: [number, number][] = [];
  for (let r = 0; r < GRID; r++) for (let c = 0; c < GRID; c++) if (!board[r][c]) empty.push([r, c]);
  if (empty.length === 0) return board;
  const [r, c] = empty[Math.floor(Math.random() * empty.length)];
  const newBoard = board.map(row => [...row]);
  newBoard[r][c] = Math.random() < 0.9 ? 2 : 4;
  return newBoard;
}

function initBoard(): Board {
  return addRandom(addRandom(createEmpty()));
}

const TILE_COLORS: Record<number, string> = {
  2: '#1e293b', 4: '#334155', 8: '#7c3aed', 16: '#6d28d9',
  32: '#a855f7', 64: '#9333ea', 128: '#c026d3', 256: '#a21caf',
  512: '#db2777', 1024: '#e11d48', 2048: '#f59e0b',
};

function slideLine(line: (number | null)[]): [number[], number] {
  const filtered = line.filter((v): v is number => v !== null);
  const merged: number[] = [];
  let score = 0;
  for (let i = 0; i < filtered.length; i++) {
    if (i + 1 < filtered.length && filtered[i] === filtered[i + 1]) {
      merged.push(filtered[i] * 2);
      score += filtered[i] * 2;
      i++;
    } else {
      merged.push(filtered[i]);
    }
  }
  while (merged.length < GRID) merged.push(0);
  return [merged, score];
}

function moveLeft(board: Board): [Board, number, boolean] {
  let changed = false;
  let totalScore = 0;
  const newBoard = board.map(row => [...row]);
  for (let r = 0; r < GRID; r++) {
    const [line, score] = slideLine(newBoard[r]);
    totalScore += score;
    for (let c = 0; c < GRID; c++) {
      if (newBoard[r][c] !== (line[c] || null)) changed = true;
      newBoard[r][c] = line[c] || null;
    }
  }
  return [newBoard, totalScore, changed];
}

function rotateLeft(board: Board): Board {
  const n = board.length;
  return board[0].map((_, i) => board.map(row => row[n - 1 - i]));
}

function rotateRight(board: Board): Board {
  return board[0].map((_, i) => board.map(row => row[i]).reverse());
}

function move(board: Board, dir: 'up' | 'down' | 'left' | 'right'): [Board, number, boolean] {
  if (dir === 'left') return moveLeft(board);
  if (dir === 'right') { const rotated = rotateRight(board); const [b, s, c] = moveLeft(rotated); return [rotateLeft(b), s, c]; }
  if (dir === 'up') { const rotated = rotateLeft(board); const [b, s, c] = moveLeft(rotated); return [rotateRight(b), s, c]; }
  const rotated = rotateLeft(rotateLeft(board));
  const [b, s, c] = moveLeft(rotated);
  return [rotateRight(rotateRight(b)), s, c];
}

function canMove(board: Board): boolean {
  for (const dir of ['up', 'down', 'left', 'right'] as const) {
    const [, , changed] = move(board, dir);
    if (changed) return true;
  }
  return false;
}

export default function Game2048({ windowId: _windowId }: { windowId: string }) {
  const [board, setBoard] = useState<Board>(initBoard);
  const [score, setScore] = useState(0);
  const [bestScore, setBestScore] = useState(() => parseInt(localStorage.getItem('2048_best') || '0'));
  const [gameOver, setGameOver] = useState(false);
  const [won, setWon] = useState(false);
  const [continuePlay, setContinuePlay] = useState(false);

  const handleMove = useCallback((dir: 'up' | 'down' | 'left' | 'right') => {
    setBoard(prev => {
      const [newBoard, points, changed] = move(prev, dir);
      if (!changed) return prev;
      const withNew = addRandom(newBoard);
      setScore(s => {
        const ns = s + points;
        setBestScore(bs => {
          const nb = Math.max(bs, ns);
          localStorage.setItem('2048_best', String(nb));
          return nb;
        });
        return ns;
      });
      if (!canMove(withNew)) setGameOver(true);
      if (!continuePlay && withNew.some(row => row.includes(2048))) setWon(true);
      return withNew;
    });
  }, [continuePlay]);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (gameOver) return;
      if (e.key === 'ArrowLeft') { e.preventDefault(); handleMove('left'); }
      if (e.key === 'ArrowRight') { e.preventDefault(); handleMove('right'); }
      if (e.key === 'ArrowUp') { e.preventDefault(); handleMove('up'); }
      if (e.key === 'ArrowDown') { e.preventDefault(); handleMove('down'); }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [handleMove, gameOver]);

  const reset = () => {
    setBoard(initBoard());
    setScore(0);
    setGameOver(false);
    setWon(false);
    setContinuePlay(false);
  };

  return (
    <div className="w-full h-full flex flex-col p-3 select-none" style={{ background: 'var(--bg-workspace)' }}>
      <div className="flex items-center justify-between mb-3">
        <div className="flex items-center gap-3 text-xs">
          <span style={{ color: 'var(--text-secondary)' }}>Score: <strong style={{ color: 'var(--accent-silver)' }}>{score}</strong></span>
          <span className="flex items-center gap-1" style={{ color: 'var(--text-muted)' }}><Trophy size={10} /> {bestScore}</span>
        </div>
        <button onClick={reset} className="p-1 rounded" style={{ background: 'var(--bg-input)', color: 'var(--accent-silver)' }}><RotateCcw size={14} /></button>
      </div>

      <div className="flex-1 flex flex-col items-center justify-center">
        <div className="grid gap-2 p-3 rounded-xl" style={{ gridTemplateColumns: `repeat(${GRID}, 1fr)`, background: 'rgba(0,0,0,0.06)' }}>
          {board.map((row, r) => row.map((cell, c) => (
            <div key={`${r}-${c}`} className="flex items-center justify-center rounded-lg font-bold text-lg transition-all"
              style={{
                width: 'clamp(60px, 12vw, 80px)', height: 'clamp(60px, 12vw, 80px)',
                background: cell ? (TILE_COLORS[cell] || '#4c1d95') : 'rgba(30,30,46,0.8)',
                color: cell && cell > 4 ? '#fff' : 'var(--text-primary)',
                fontSize: cell && cell >= 1000 ? '1.2rem' : '1.5rem',
                boxShadow: cell ? `0 0 12px ${TILE_COLORS[cell] || '#4c1d95'}40` : 'none',
              }}
            >
              {cell || ''}
            </div>
          )))}
        </div>
      </div>

      <div className="flex justify-center gap-2 mt-3">
        {(['left', 'right', 'up', 'down'] as const).map(dir => (
          <button key={dir} onClick={() => handleMove(dir)}
            className="px-3 py-1.5 rounded-lg text-xs font-medium capitalize"
            style={{ background: 'var(--bg-input)', color: 'var(--text-primary)' }}
          >{dir}</button>
        ))}
      </div>

      {(gameOver || won) && (
        <div className="absolute inset-0 z-50 flex items-center justify-center bg-black/60 rounded-lg">
          <div className="p-5 rounded-xl text-center" style={{ background: 'var(--bg-window)' }}>
            <h2 className="text-lg font-bold mb-2" style={{ color: won ? 'var(--success)' : 'var(--error)' }}>{won ? 'You reached 2048!' : 'Game Over'}</h2>
            <p className="text-xs mb-3" style={{ color: 'var(--text-muted)' }}>Score: {score}</p>
            {won ? (
              <div className="flex gap-2 justify-center">
                <button onClick={() => setWon(false)} className="px-4 py-2 rounded-lg text-sm font-medium text-white" style={{ background: 'var(--accent-dark-gray)' }}>Continue</button>
                <button onClick={reset} className="px-4 py-2 rounded-lg text-sm font-medium" style={{ background: 'var(--bg-input)', color: 'var(--text-primary)' }}>New Game</button>
              </div>
            ) : (
              <button onClick={reset} className="px-4 py-2 rounded-lg text-sm font-medium text-white" style={{ background: 'var(--accent-dark-gray)' }}>New Game</button>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
