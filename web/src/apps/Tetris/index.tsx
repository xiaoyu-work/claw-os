import { useState, useEffect, useCallback, useRef } from 'react';
import { RotateCcw, ChevronDown } from 'lucide-react';

const COLS = 10;
const ROWS = 20;
const COLORS: Record<string, string> = {
  I: '#06B6D4', O: '#F59E0B', T: '#A855F7', S: '#22C55E',
  Z: '#EF4444', J: '#5A7A8A', L: '#F97316',
};

const SHAPES: Record<string, number[][][]> = {
  I: [[[1,1,1,1]],[[1],[1],[1],[1]]],
  O: [[[1,1],[1,1]]],
  T: [[[0,1,0],[1,1,1]],[[1,0],[1,1],[1,0]],[[1,1,1],[0,1,0]],[[0,1],[1,1],[0,1]]],
  S: [[[0,1,1],[1,1,0]],[[1,0],[1,1],[0,1]]],
  Z: [[[1,1,0],[0,1,1]],[[0,1],[1,1],[1,0]]],
  J: [[[1,0,0],[1,1,1]],[[1,1],[1,0],[1,0]],[[1,1,1],[0,0,1]],[[0,1],[0,1],[1,1]]],
  L: [[[0,0,1],[1,1,1]],[[1,0],[1,0],[1,1]],[[1,1,1],[1,0,0]],[[1,1],[0,1],[0,1]]],
};

const PIECE_NAMES = Object.keys(SHAPES);

interface Piece { type: string; shape: number[][]; x: number; y: number; rotation: number }

function createPiece(type: string): Piece {
  return { type, shape: SHAPES[type][0], x: 4 - Math.floor(SHAPES[type][0][0].length / 2), y: 0, rotation: 0 };
}

function randomPiece(): Piece {
  return createPiece(PIECE_NAMES[Math.floor(Math.random() * PIECE_NAMES.length)]);
}

function rotatePiece(p: Piece): Piece {
  const shapes = SHAPES[p.type];
  const newRotation = (p.rotation + 1) % shapes.length;
  return { ...p, shape: shapes[newRotation], rotation: newRotation };
}

function isValid(board: number[][], piece: Piece): boolean {
  for (let r = 0; r < piece.shape.length; r++) {
    for (let c = 0; c < piece.shape[r].length; c++) {
      if (piece.shape[r][c]) {
        const nr = piece.y + r, nc = piece.x + c;
        if (nr < 0 || nr >= ROWS || nc < 0 || nc >= COLS || board[nr][nc]) return false;
      }
    }
  }
  return true;
}

function placePiece(board: number[][], piece: Piece): number[][] {
  const newBoard = board.map(r => [...r]);
  for (let r = 0; r < piece.shape.length; r++) {
    for (let c = 0; c < piece.shape[r].length; c++) {
      if (piece.shape[r][c]) newBoard[piece.y + r][piece.x + c] = 1;
    }
  }
  return newBoard;
}

function clearLines(board: number[][]): [number[][], number] {
  const newBoard = board.filter(row => row.some(cell => !cell));
  const cleared = ROWS - newBoard.length;
  while (newBoard.length < ROWS) newBoard.unshift(Array(COLS).fill(0));
  return [newBoard, cleared];
}

export default function Tetris({ windowId: _windowId }: { windowId: string }) {
  const [board, setBoard] = useState<number[][]>(() => Array.from({ length: ROWS }, () => Array(COLS).fill(0)));
  const [current, setCurrent] = useState<Piece>(randomPiece);
  const [next, setNext] = useState<Piece>(randomPiece);
  const [score, setScore] = useState(0);
  const [level, setLevel] = useState(1);
  const [lines, setLines] = useState(0);
  const [gameOver, setGameOver] = useState(false);
  const [paused, setPaused] = useState(false);
  const dropRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const boardRef = useRef(board);
  boardRef.current = board;
  const currentRef = useRef(current);
  currentRef.current = current;
  const nextRef = useRef(next);
  nextRef.current = next;
  const pausedRef = useRef(paused);
  pausedRef.current = paused;
  const gameOverRef = useRef(gameOver);
  gameOverRef.current = gameOver;

  const getSpeed = useCallback(() => Math.max(100, 800 - (level - 1) * 80), [level]);

  const lockPiece = useCallback(() => {
    const newBoard = placePiece(boardRef.current, currentRef.current);
    const [clearedBoard, linesCleared] = clearLines(newBoard);
    if (linesCleared > 0) {
      setLines(l => l + linesCleared);
      setScore(s => s + [0, 100, 300, 500, 800][linesCleared] * level);
      setLevel(Math.floor((lines + linesCleared) / 10) + 1);
    }
    const newPiece = nextRef.current;
    setCurrent(newPiece);
    setNext(randomPiece());
    if (!isValid(clearedBoard, newPiece)) {
      setGameOver(true);
      gameOverRef.current = true;
    }
    setBoard(clearedBoard);
  }, [lines, level]);

  useEffect(() => {
    if (gameOver || paused) return;
    dropRef.current = setInterval(() => {
      if (pausedRef.current || gameOverRef.current) return;
      const moved = { ...currentRef.current, y: currentRef.current.y + 1 };
      if (isValid(boardRef.current, moved)) {
        setCurrent(moved);
      } else {
        lockPiece();
      }
    }, getSpeed());
    return () => { if (dropRef.current) clearInterval(dropRef.current); };
  }, [gameOver, paused, getSpeed, lockPiece]);

  const move = useCallback((dx: number, dy: number) => {
    if (gameOverRef.current || pausedRef.current) return;
    const moved = { ...currentRef.current, x: currentRef.current.x + dx, y: currentRef.current.y + dy };
    if (isValid(boardRef.current, moved)) setCurrent(moved);
  }, []);

  const rotate = useCallback(() => {
    if (gameOverRef.current || pausedRef.current) return;
    const rotated = rotatePiece(currentRef.current);
    if (isValid(boardRef.current, rotated)) { setCurrent(rotated); return; }
    for (const offset of [-1, 1, -2, 2]) {
      const kicked = { ...rotated, x: rotated.x + offset };
      if (isValid(boardRef.current, kicked)) { setCurrent(kicked); return; }
    }
  }, []);

  const hardDrop = useCallback(() => {
    if (gameOverRef.current || pausedRef.current) return;
    let dropY = currentRef.current.y;
    while (isValid(boardRef.current, { ...currentRef.current, y: dropY + 1 })) dropY++;
    setCurrent({ ...currentRef.current, y: dropY });
    setTimeout(() => lockPiece(), 50);
  }, [lockPiece]);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (gameOverRef.current) return;
      if (e.key === 'ArrowLeft') { e.preventDefault(); move(-1, 0); }
      if (e.key === 'ArrowRight') { e.preventDefault(); move(1, 0); }
      if (e.key === 'ArrowDown') { e.preventDefault(); move(0, 1); }
      if (e.key === 'ArrowUp') { e.preventDefault(); rotate(); }
      if (e.key === ' ') { e.preventDefault(); hardDrop(); }
      if (e.key === 'p' || e.key === 'P') { e.preventDefault(); setPaused(p => !p); }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [move, rotate, hardDrop]);

  const reset = () => {
    setBoard(Array.from({ length: ROWS }, () => Array(COLS).fill(0)));
    const p = randomPiece();
    setCurrent(p);
    setNext(randomPiece());
    setScore(0);
    setLevel(1);
    setLines(0);
    setGameOver(false);
    setPaused(false);
  };

  const displayBoard = Array.from({ length: ROWS }, (_, r) =>
    Array.from({ length: COLS }, (_, c) => {
      if (board[r][c]) return { filled: true, color: 'var(--bg-hover)' };
      for (let pr = 0; pr < current.shape.length; pr++) {
        for (let pc = 0; pc < current.shape[pr].length; pc++) {
          if (current.shape[pr][pc] && current.y + pr === r && current.x + pc === c) {
            return { filled: true, color: COLORS[current.type] || 'var(--accent-silver)' };
          }
        }
      }
      return null;
    })
  );

  return (
    <div className="w-full h-full flex flex-col p-3 select-none" style={{ background: 'var(--bg-workspace)' }} tabIndex={0}>
      <div className="flex items-center justify-between mb-2">
        <div className="flex items-center gap-3">
          <span className="text-xs font-medium" style={{ color: 'var(--text-secondary)' }}>Score: <span style={{ color: 'var(--accent-silver)' }}>{score}</span></span>
          <span className="text-xs font-medium" style={{ color: 'var(--text-secondary)' }}>Level: <span style={{ color: 'var(--accent-silver)' }}>{level}</span></span>
          <span className="text-xs font-medium" style={{ color: 'var(--text-secondary)' }}>Lines: <span style={{ color: 'var(--accent-silver)' }}>{lines}</span></span>
        </div>
        <div className="flex gap-1">
          <button onClick={() => setPaused(p => !p)} className="px-2 py-1 rounded text-xs" style={{ background: 'var(--bg-input)', color: 'var(--text-primary)' }}>{paused ? 'Resume' : 'Pause'}</button>
          <button onClick={reset} className="p-1 rounded" style={{ background: 'var(--bg-input)', color: 'var(--accent-silver)' }}><RotateCcw size={14} /></button>
        </div>
      </div>
      <div className="flex flex-1 gap-3 min-h-0">
        <div className="flex-1 flex items-center justify-center">
          <div className="grid gap-px rounded overflow-hidden" style={{ gridTemplateColumns: `repeat(${COLS}, 1fr)`, background: 'rgba(0,0,0,0.12)' }}>
            {displayBoard.map((row, r) => row.map((cell, c) => (
              <div key={`${r}-${c}`} className="transition-colors" style={{
                width: 'clamp(14px, 3vw, 24px)', height: 'clamp(14px, 3vw, 24px)',
                background: cell ? cell.color : 'var(--bg-workspace)',
                borderRadius: 1,
                boxShadow: cell ? 'inset 0 1px 0 rgba(255,255,255,0.2)' : 'none',
              }} />
            )))}
          </div>
        </div>
        <div className="w-20 flex flex-col gap-2">
          <div className="p-2 rounded-lg" style={{ background: 'var(--bg-window)' }}>
            <div className="text-[10px] font-medium mb-1" style={{ color: 'var(--accent-silver)' }}>Next</div>
            <div className="grid gap-px" style={{ gridTemplateColumns: `repeat(${next.shape[0]?.length || 2}, 1fr)` }}>
              {next.shape.flatMap((row, r) => row.map((cell, c) => (
                <div key={`n-${r}-${c}`} style={{ width: 14, height: 14, background: cell ? COLORS[next.type] : 'transparent', borderRadius: 1 }} />
              )))}
            </div>
          </div>
          <div className="text-[10px] p-2 rounded-lg" style={{ background: 'var(--bg-window)', color: 'var(--text-muted)' }}>
            <div className="mb-1"><ChevronDown size={10} className="inline" /> Drop</div>
            <div className="mb-1">↑ Rotate</div>
            <div>P Pause</div>
          </div>
        </div>
      </div>
      {(gameOver || paused) && (
        <div className="absolute inset-0 z-50 flex items-center justify-center bg-black/60 rounded-lg">
          <div className="p-5 rounded-xl text-center" style={{ background: 'var(--bg-window)' }}>
            <h2 className="text-lg font-bold mb-2" style={{ color: gameOver ? 'var(--error)' : 'var(--accent-silver)' }}>{gameOver ? 'Game Over' : 'Paused'}</h2>
            {gameOver && <p className="text-xs mb-3" style={{ color: 'var(--text-muted)' }}>Score: {score}</p>}
            <button onClick={gameOver ? reset : () => setPaused(false)} className="px-4 py-2 rounded-lg text-sm font-medium text-white" style={{ background: 'var(--accent-dark-gray)' }}>{gameOver ? 'New Game' : 'Resume'}</button>
          </div>
        </div>
      )}
    </div>
  );
}
