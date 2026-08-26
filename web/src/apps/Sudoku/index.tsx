import { useState, useCallback, useEffect } from 'react';
import { RotateCcw, Lightbulb, Wand2, Clock, Eraser, Pencil } from 'lucide-react';

const GRID = 9;
const BOX = 3;

type Cell = { value: number | null; fixed: boolean; notes: Set<number> };
type Board = Cell[][];
type Difficulty = 'easy' | 'medium' | 'hard';

function createEmptyBoard(): Board {
  return Array.from({ length: GRID }, () =>
    Array.from({ length: GRID }, () => ({ value: null, fixed: false, notes: new Set<number>() }))
  );
}

function isValid(board: Board, row: number, col: number, num: number): boolean {
  for (let i = 0; i < GRID; i++) {
    if (board[row][i].value === num && i !== col) return false;
    if (board[i][col].value === num && i !== row) return false;
  }
  const boxRow = Math.floor(row / BOX) * BOX;
  const boxCol = Math.floor(col / BOX) * BOX;
  for (let r = boxRow; r < boxRow + BOX; r++) {
    for (let c = boxCol; c < boxCol + BOX; c++) {
      if (board[r][c].value === num && (r !== row || c !== col)) return false;
    }
  }
  return true;
}

function solveBoard(board: Board): boolean {
  for (let r = 0; r < GRID; r++) {
    for (let c = 0; c < GRID; c++) {
      if (!board[r][c].value) {
        for (let num = 1; num <= GRID; num++) {
          if (isValid(board, r, c, num)) {
            board[r][c].value = num;
            if (solveBoard(board)) return true;
            board[r][c].value = null;
          }
        }
        return false;
      }
    }
  }
  return true;
}

function generatePuzzle(difficulty: Difficulty): Board {
  const board = createEmptyBoard();
  for (let i = 0; i < GRID; i += BOX) {
    const nums = [1, 2, 3, 4, 5, 6, 7, 8, 9];
    for (let j = nums.length - 1; j > 0; j--) { const k = Math.floor(Math.random() * (j + 1)); [nums[j], nums[k]] = [nums[k], nums[j]]; }
    for (let r = 0; r < BOX; r++) for (let c = 0; c < BOX; c++) board[i + r][i + c].value = nums[r * BOX + c];
  }
  solveBoard(board);
  const cells: [number, number][] = [];
  for (let r = 0; r < GRID; r++) for (let c = 0; c < GRID; c++) cells.push([r, c]);
  for (let i = cells.length - 1; i > 0; i--) { const j = Math.floor(Math.random() * (i + 1)); [cells[i], cells[j]] = [cells[j], cells[i]]; }
  const removeCount = difficulty === 'easy' ? 35 : difficulty === 'medium' ? 45 : 55;
  for (let i = 0; i < removeCount; i++) {
    const [r, c] = cells[i];
    board[r][c].value = null;
  }
  for (let r = 0; r < GRID; r++) for (let c = 0; c < GRID; c++) if (board[r][c].value) board[r][c].fixed = true;
  return board;
}

export default function Sudoku({ windowId: _windowId }: { windowId: string }) {
  const [difficulty, setDifficulty] = useState<Difficulty>('medium');
  const [board, setBoard] = useState<Board>(() => generatePuzzle('medium'));
  const [selected, setSelected] = useState<[number, number] | null>(null);
  const [notesMode, setNotesMode] = useState(false);
  const [timer, setTimer] = useState(0);
  const [conflicts, setConflicts] = useState<Set<string>>(new Set());
  const [solved, setSolved] = useState(false);

  useEffect(() => {
    if (solved) return;
    const interval = setInterval(() => setTimer(t => t + 1), 1000);
    return () => clearInterval(interval);
  }, [solved]);

  const checkConflicts = useCallback((brd: Board) => {
    const newConflicts = new Set<string>();
    for (let r = 0; r < GRID; r++) {
      for (let c = 0; c < GRID; c++) {
        if (!brd[r][c].value) continue;
        if (!isValid(brd, r, c, brd[r][c].value!)) newConflicts.add(`${r}-${c}`);
      }
    }
    setConflicts(newConflicts);
  }, []);

  const handleCellClick = useCallback((row: number, col: number) => {
    if (board[row][col].fixed) return;
    setSelected([row, col]);
  }, [board]);

  const handleNumber = useCallback((num: number) => {
    if (!selected) return;
    const [r, c] = selected;
    if (board[r][c].fixed) return;
    setBoard(prev => {
      const newBoard = prev.map(row => row.map(cell => ({ ...cell, notes: new Set(cell.notes) })));
      if (notesMode) {
        if (newBoard[r][c].notes.has(num)) newBoard[r][c].notes.delete(num);
        else newBoard[r][c].notes.add(num);
      } else {
        newBoard[r][c].value = num;
        newBoard[r][c].notes.clear();
        if (newBoard.every(row => row.every(cell => cell.value))) {
          const allValid = newBoard.every((row, ri) => row.every((cell, ci) => isValid(newBoard, ri, ci, cell.value!)));
          if (allValid) setSolved(true);
        }
      }
      checkConflicts(newBoard);
      return newBoard;
    });
  }, [selected, board, notesMode, checkConflicts]);

  const handleErase = useCallback(() => {
    if (!selected) return;
    const [r, c] = selected;
    if (board[r][c].fixed) return;
    setBoard(prev => {
      const newBoard = prev.map(row => row.map(cell => ({ ...cell, notes: new Set(cell.notes) })));
      newBoard[r][c].value = null;
      checkConflicts(newBoard);
      return newBoard;
    });
  }, [selected, board, checkConflicts]);

  const handleHint = useCallback(() => {
    if (!selected) return;
    const [r, c] = selected;
    if (board[r][c].fixed || board[r][c].value) return;
    const solution = board.map(row => row.map(cell => ({ ...cell })));
    solveBoard(solution);
    setBoard(prev => {
      const newBoard = prev.map(row => row.map(cell => ({ ...cell, notes: new Set(cell.notes) })));
      newBoard[r][c].value = solution[r][c].value;
      checkConflicts(newBoard);
      return newBoard;
    });
  }, [selected, board, checkConflicts]);

  const handleSolve = useCallback(() => {
    setBoard(prev => {
      const newBoard = prev.map(row => row.map(cell => ({ ...cell, notes: new Set(cell.notes) })));
      solveBoard(newBoard);
      checkConflicts(newBoard);
      return newBoard;
    });
  }, [checkConflicts]);

  const newPuzzle = useCallback(() => {
    setBoard(generatePuzzle(difficulty));
    setSelected(null);
    setConflicts(new Set());
    setSolved(false);
    setTimer(0);
  }, [difficulty]);

  useEffect(() => { newPuzzle(); }, [difficulty, newPuzzle]);

  const formatTime = (s: number) => `${Math.floor(s / 60)}:${(s % 60).toString().padStart(2, '0')}`;

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key >= '1' && e.key <= '9') handleNumber(parseInt(e.key));
      if (e.key === 'Backspace' || e.key === 'Delete') handleErase();
      if (e.key === 'n') setNotesMode(p => !p);
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [handleNumber, handleErase]);

  return (
    <div className="w-full h-full flex flex-col p-3 select-none" style={{ background: 'var(--bg-workspace)' }}>
      <div className="flex items-center justify-between mb-2">
        <div className="flex gap-1">
          {(['easy', 'medium', 'hard'] as Difficulty[]).map(d => (
            <button key={d} onClick={() => setDifficulty(d)} className="px-2 py-1 rounded text-xs font-medium capitalize"
              style={{ background: difficulty === d ? 'var(--accent-dark-gray)' : 'var(--bg-input)', color: 'var(--text-primary)' }}>{d}</button>
          ))}
        </div>
        <div className="flex items-center gap-2 text-xs" style={{ color: 'var(--text-secondary)' }}>
          <span className="flex items-center gap-1"><Clock size={10} /> {formatTime(timer)}</span>
          <button onClick={newPuzzle} className="p-1 rounded" style={{ background: 'var(--bg-input)', color: 'var(--accent-silver)' }}><RotateCcw size={12} /></button>
        </div>
      </div>

      <div className="flex flex-1 gap-3 min-h-0">
        <div className="flex-1 flex items-center justify-center">
          <div className="grid gap-px rounded-lg overflow-hidden" style={{ gridTemplateColumns: `repeat(${GRID}, 1fr)`, background: 'rgba(0,0,0,0.12)' }}>
            {board.map((row, r) => row.map((cell, c) => {
              const isSelected = selected?.[0] === r && selected?.[1] === c;
              const hasConflict = conflicts.has(`${r}-${c}`);
              const isSameRow = selected?.[0] === r;
              const isSameCol = selected?.[1] === c;
              const isSameBox = selected && Math.floor(selected[0] / BOX) === Math.floor(r / BOX) && Math.floor(selected[1] / BOX) === Math.floor(c / BOX);
              const highlight = isSelected || isSameRow || isSameCol || isSameBox;
              return (
                <button key={`${r}-${c}`} onClick={() => handleCellClick(r, c)}
                  className="relative flex items-center justify-center font-bold text-sm transition-all"
                  style={{
                    width: 'clamp(28px, 7vw, 38px)', height: 'clamp(28px, 7vw, 38px)',
                    background: hasConflict ? 'rgba(239,68,68,0.2)' : highlight ? 'rgba(125,139,150,0.1)' : (r % BOX === 0 && r > 0) || (c % BOX === 0 && c > 0) ? 'var(--bg-input)' : 'var(--bg-window)',
                    borderRight: c % BOX === BOX - 1 && c < GRID - 1 ? '2px solid rgba(0,0,0,0.18)' : '1px solid rgba(0,0,0,0.04)',
                    borderBottom: r % BOX === BOX - 1 && r < GRID - 1 ? '2px solid rgba(0,0,0,0.18)' : '1px solid rgba(0,0,0,0.04)',
                    color: hasConflict ? 'var(--error)' : cell.fixed ? 'var(--text-primary)' : 'var(--accent-silver)',
                    fontWeight: cell.fixed ? 700 : 500,
                  }}
                >
                  {cell.value || ''}
                  {cell.notes.size > 0 && !cell.value && (
                    <div className="absolute inset-0 grid grid-cols-3 gap-0 p-0.5">
                      {Array.from({ length: 9 }, (_, i) => (
                        <div key={i} className="flex items-center justify-center text-[6px] leading-none" style={{ color: cell.notes.has(i + 1) ? 'var(--text-muted)' : 'transparent' }}>{i + 1}</div>
                      ))}
                    </div>
                  )}
                </button>
              );
            }))}
          </div>
        </div>

        <div className="w-12 flex flex-col gap-1">
          {Array.from({ length: 9 }, (_, i) => (
            <button key={i + 1} onClick={() => handleNumber(i + 1)}
              className="h-8 rounded text-sm font-medium flex items-center justify-center transition-all hover:bg-[var(--bg-hover)]"
              style={{ background: 'var(--bg-input)', color: 'var(--text-primary)' }}>{i + 1}</button>
          ))}
        </div>
      </div>

      <div className="flex items-center justify-center gap-2 mt-2">
        <button onClick={() => setNotesMode(!notesMode)} className="px-2 py-1 rounded text-xs flex items-center gap-1"
          style={{ background: notesMode ? 'var(--accent-dark-gray)' : 'var(--bg-input)', color: 'var(--text-primary)' }}><Pencil size={10} /> Notes</button>
        <button onClick={handleErase} className="px-2 py-1 rounded text-xs flex items-center gap-1" style={{ background: 'var(--bg-input)', color: 'var(--text-primary)' }}><Eraser size={10} /> Clear</button>
        <button onClick={handleHint} className="px-2 py-1 rounded text-xs flex items-center gap-1" style={{ background: 'var(--bg-input)', color: 'var(--text-primary)' }}><Lightbulb size={10} /> Hint</button>
        <button onClick={handleSolve} className="px-2 py-1 rounded text-xs flex items-center gap-1" style={{ background: 'var(--bg-input)', color: 'var(--text-primary)' }}><Wand2 size={10} /> Solve</button>
      </div>

      {solved && (
        <div className="absolute inset-0 z-50 flex items-center justify-center bg-black/60 rounded-lg">
          <div className="p-5 rounded-xl text-center" style={{ background: 'var(--bg-window)' }}>
            <h2 className="text-lg font-bold mb-2" style={{ color: 'var(--success)' }}>Solved!</h2>
            <p className="text-xs mb-3" style={{ color: 'var(--text-muted)' }}>Time: {formatTime(timer)}</p>
            <button onClick={newPuzzle} className="px-4 py-2 rounded-lg text-sm font-medium text-white" style={{ background: 'var(--accent-dark-gray)' }}>New Puzzle</button>
          </div>
        </div>
      )}
    </div>
  );
}
