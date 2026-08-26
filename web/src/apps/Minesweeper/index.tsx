import { useState, useCallback, useEffect, useRef } from 'react';
import { RotateCcw, Clock, Bomb, Flag } from 'lucide-react';

type CellState = { mine: boolean; revealed: boolean; flagged: boolean; adjacent: number };
type Difficulty = { rows: number; cols: number; mines: number; name: string };

const DIFFICULTIES: Record<string, Difficulty> = {
  easy: { rows: 9, cols: 9, mines: 10, name: 'Easy' },
  medium: { rows: 16, cols: 16, mines: 40, name: 'Medium' },
  hard: { rows: 16, cols: 30, mines: 99, name: 'Hard' },
};

function createBoard(diff: Difficulty): CellState[][] {
  const board: CellState[][] = Array.from({ length: diff.rows }, () =>
    Array.from({ length: diff.cols }, () => ({ mine: false, revealed: false, flagged: false, adjacent: 0 }))
  );
  let placed = 0;
  while (placed < diff.mines) {
    const r = Math.floor(Math.random() * diff.rows);
    const c = Math.floor(Math.random() * diff.cols);
    if (!board[r][c].mine) { board[r][c].mine = true; placed++; }
  }
  for (let r = 0; r < diff.rows; r++) {
    for (let c = 0; c < diff.cols; c++) {
      if (!board[r][c].mine) {
        let count = 0;
        for (let dr = -1; dr <= 1; dr++) for (let dc = -1; dc <= 1; dc++) {
          const nr = r + dr, nc = c + dc;
          if (nr >= 0 && nr < diff.rows && nc >= 0 && nc < diff.cols && board[nr][nc].mine) count++;
        }
        board[r][c].adjacent = count;
      }
    }
  }
  return board;
}

export default function Minesweeper({ windowId: _windowId }: { windowId: string }) {
  const [difficulty, setDifficulty] = useState<string>('easy');
  const [board, setBoard] = useState<CellState[][]>(() => createBoard(DIFFICULTIES.easy));
  const [gameOver, setGameOver] = useState(false);
  const [won, setWon] = useState(false);
  const [timer, setTimer] = useState(0);
  const [flagCount, setFlagCount] = useState(0);
  const [started, setStarted] = useState(false);
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const diff = DIFFICULTIES[difficulty];

  useEffect(() => {
    if (started && !gameOver && !won) {
      timerRef.current = setInterval(() => setTimer(t => t + 1), 1000);
      return () => { if (timerRef.current) clearInterval(timerRef.current); };
    }
  }, [started, gameOver, won]);

  const checkWin = useCallback((brd: CellState[][]) => {
    const totalCells = diff.rows * diff.cols;
    let revealedCount = 0;
    for (let r = 0; r < diff.rows; r++) for (let c = 0; c < diff.cols; c++) if (brd[r][c].revealed) revealedCount++;
    if (revealedCount === totalCells - diff.mines) { setWon(true); setGameOver(true); }
  }, [diff]);

  const floodFill = useCallback((brd: CellState[][], r: number, c: number) => {
    const rows = brd.length, cols = brd[0].length;
    const stack: [number, number][] = [[r, c]];
    while (stack.length > 0) {
      const [cr, cc] = stack.pop()!;
      if (cr < 0 || cr >= rows || cc < 0 || cc >= cols || brd[cr][cc].revealed || brd[cr][cc].flagged || brd[cr][cc].mine) continue;
      brd[cr][cc].revealed = true;
      if (brd[cr][cc].adjacent === 0) {
        for (let dr = -1; dr <= 1; dr++) for (let dc = -1; dc <= 1; dc++) stack.push([cr + dr, cc + dc]);
      }
    }
  }, []);

  const handleReveal = useCallback((row: number, col: number) => {
    if (gameOver || won) return;
    if (!started) setStarted(true);
    setBoard(prev => {
      const brd = prev.map(r => r.map(c => ({ ...c })));
      if (brd[row][col].flagged || brd[row][col].revealed) return prev;
      if (brd[row][col].mine) {
        for (let r = 0; r < diff.rows; r++) for (let c = 0; c < diff.cols; c++) if (brd[r][c].mine) brd[r][c].revealed = true;
        setGameOver(true);
        return brd;
      }
      floodFill(brd, row, col);
      checkWin(brd);
      return brd;
    });
  }, [gameOver, won, started, diff, floodFill, checkWin]);

  const handleFlag = useCallback((e: React.MouseEvent, row: number, col: number) => {
    e.preventDefault();
    if (gameOver || won) return;
    if (!started) setStarted(true);
    setBoard(prev => {
      const brd = prev.map(r => r.map(c => ({ ...c })));
      if (brd[row][col].revealed) return prev;
      brd[row][col].flagged = !brd[row][col].flagged;
      setFlagCount(f => brd[row][col].flagged ? f + 1 : f - 1);
      return brd;
    });
  }, [gameOver, won, started]);

  const newGame = useCallback(() => {
    setBoard(createBoard(diff));
    setGameOver(false);
    setWon(false);
    setTimer(0);
    setFlagCount(0);
    setStarted(false);
    if (timerRef.current) clearInterval(timerRef.current);
  }, [diff]);

  useEffect(() => { newGame(); }, [difficulty, newGame]);

  const cellSize = diff.cols > 16 ? 'w-5 h-5 text-[9px]' : diff.cols > 9 ? 'w-6 h-6 text-xs' : 'w-7 h-7 text-sm';
  const adjColors = ['', 'text-blue-400', 'text-green-400', 'text-red-400', 'text-purple-400', 'text-yellow-400', 'text-pink-400', 'text-gray-400', 'text-white'];

  return (
    <div className="w-full h-full flex flex-col p-3 select-none" style={{ background: 'var(--bg-workspace)' }}>
      <div className="flex items-center justify-between mb-3">
        <div className="flex gap-1">
          {Object.entries(DIFFICULTIES).map(([key, d]) => (
            <button key={key} onClick={() => setDifficulty(key)} className="px-2 py-1 rounded text-xs font-medium transition-all"
              style={{ background: difficulty === key ? 'var(--accent-dark-gray)' : 'var(--bg-input)', color: 'var(--text-primary)' }}>{d.name}</button>
          ))}
        </div>
        <div className="flex items-center gap-3 text-xs" style={{ color: 'var(--text-secondary)' }}>
          <span className="flex items-center gap-1"><Flag size={12} /> {diff.mines - flagCount}</span>
          <span className="flex items-center gap-1"><Clock size={12} /> {timer}</span>
          <button onClick={newGame} className="p-1.5 rounded-lg hover:bg-[var(--bg-hover)] transition-all" style={{ color: 'var(--accent-silver)' }}><RotateCcw size={14} /></button>
        </div>
      </div>
      <div className="flex-1 flex items-center justify-center overflow-auto">
        <div className="grid gap-0.5" style={{ gridTemplateColumns: `repeat(${diff.cols}, 1fr)` }}>
          {board.map((row, r) => row.map((cell, c) => (
            <button
              key={`${r}-${c}`}
              onClick={() => handleReveal(r, c)}
              onContextMenu={(e) => handleFlag(e, r, c)}
              className={`${cellSize} rounded-sm font-bold flex items-center justify-center transition-all`}
              style={{
                background: cell.revealed ? (cell.mine ? 'var(--error)' : 'rgba(0,0,0,0.10)') : 'var(--bg-input)',
                border: cell.revealed ? 'none' : '1px solid rgba(0,0,0,0.10)',
              }}
            >
              {cell.revealed && cell.mine && <Bomb size={diff.cols > 16 ? 10 : 12} />}
              {cell.revealed && !cell.mine && cell.adjacent > 0 && <span className={adjColors[cell.adjacent]}>{cell.adjacent}</span>}
              {!cell.revealed && cell.flagged && <Flag size={diff.cols > 16 ? 10 : 12} style={{ color: 'var(--error)' }} />}
            </button>
          )))}
        </div>
      </div>
      {(gameOver || won) && (
        <div className="absolute inset-0 z-50 flex items-center justify-center bg-black/60 rounded-lg">
          <div className="p-5 rounded-xl text-center" style={{ background: 'var(--bg-window)' }}>
            <h2 className="text-lg font-bold mb-2" style={{ color: won ? 'var(--success)' : 'var(--error)' }}>{won ? 'You Win!' : 'Game Over!'}</h2>
            <p className="text-xs mb-3" style={{ color: 'var(--text-muted)' }}>Time: {timer}s</p>
            <button onClick={newGame} className="px-4 py-2 rounded-lg text-sm font-medium text-white" style={{ background: 'var(--accent-dark-gray)' }}>New Game</button>
          </div>
        </div>
      )}
    </div>
  );
}
