import { useState, useCallback, useEffect } from 'react';
import { RotateCcw, User, Bot, Users } from 'lucide-react';

type Cell = 'X' | 'O' | null;
type Mode = 'pvp' | 'easy' | 'hard';

const WIN_LINES = [
  [0, 1, 2], [3, 4, 5], [6, 7, 8],
  [0, 3, 6], [1, 4, 7], [2, 5, 8],
  [0, 4, 8], [2, 4, 6],
];

function checkWinner(board: Cell[]): { winner: Cell; line: number[] | null } {
  for (const line of WIN_LINES) {
    const [a, b, c] = line;
    if (board[a] && board[a] === board[b] && board[a] === board[c]) return { winner: board[a], line };
  }
  if (board.every(c => c !== null)) return { winner: null, line: null };
  return { winner: null, line: null };
}

function minimax(board: Cell[], depth: number, isMax: boolean): number {
  const { winner } = checkWinner(board);
  if (winner === 'O') return 10 - depth;
  if (winner === 'X') return depth - 10;
  if (board.every(c => c !== null)) return 0;

  if (isMax) {
    let best = -Infinity;
    for (let i = 0; i < 9; i++) {
      if (!board[i]) {
        board[i] = 'O';
        best = Math.max(best, minimax(board, depth + 1, false));
        board[i] = null;
      }
    }
    return best;
  } else {
    let best = Infinity;
    for (let i = 0; i < 9; i++) {
      if (!board[i]) {
        board[i] = 'X';
        best = Math.min(best, minimax(board, depth + 1, true));
        board[i] = null;
      }
    }
    return best;
  }
}

function getBestMove(board: Cell[]): number {
  let bestVal = -Infinity;
  let bestMove = -1;
  for (let i = 0; i < 9; i++) {
    if (!board[i]) {
      board[i] = 'O';
      const val = minimax(board, 0, false);
      board[i] = null;
      if (val > bestVal) { bestVal = val; bestMove = i; }
    }
  }
  return bestMove;
}

function getEasyMove(board: Cell[]): number {
  const empty = board.map((c, i) => c === null ? i : -1).filter(i => i !== -1);
  return empty[Math.floor(Math.random() * empty.length)];
}

export default function TicTacToe({ windowId: _windowId }: { windowId: string }) {
  const [board, setBoard] = useState<Cell[]>(Array(9).fill(null));
  const [turn, setTurn] = useState<'X' | 'O'>('X');
  const [mode, setMode] = useState<Mode>('pvp');
  const [winner, setWinner] = useState<{ winner: Cell; line: number[] | null } | null>(null);
  const [scores, setScores] = useState({ X: 0, O: 0, draw: 0 });

  const resetBoard = useCallback(() => {
    setBoard(Array(9).fill(null));
    setTurn('X');
    setWinner(null);
  }, []);

  const handleClick = useCallback((index: number) => {
    if (board[index] || winner) return;
    const newBoard = [...board];
    newBoard[index] = turn;
    setBoard(newBoard);
    const result = checkWinner(newBoard);
    if (result.winner || result.line === null && newBoard.every(c => c !== null)) {
      setWinner(result);
      setScores(s => ({
        X: s.X + (result.winner === 'X' ? 1 : 0),
        O: s.O + (result.winner === 'O' ? 1 : 0),
        draw: s.draw + (result.winner === null ? 1 : 0),
      }));
      return;
    }
    setTurn(turn === 'X' ? 'O' : 'X');
  }, [board, turn, winner]);

  useEffect(() => {
    if (mode !== 'pvp' && turn === 'O' && !winner) {
      const timer = setTimeout(() => {
        const move = mode === 'hard' ? getBestMove([...board]) : getEasyMove([...board]);
        if (move !== -1) handleClick(move);
      }, 400);
      return () => clearTimeout(timer);
    }
  }, [turn, mode, winner, board, handleClick]);

  return (
    <div className="w-full h-full flex flex-col p-3 select-none" style={{ background: 'var(--bg-workspace)' }}>
      <div className="flex items-center justify-between mb-3">
        <div className="flex gap-1">
          {([['pvp', Users], ['easy', User], ['hard', Bot]] as [Mode, typeof User][]).map(([m, Icon]) => (
            <button key={m} onClick={() => { setMode(m); resetBoard(); }}
              className="px-2 py-1 rounded text-xs font-medium flex items-center gap-1 transition-all"
              style={{ background: mode === m ? 'var(--accent-dark-gray)' : 'var(--bg-input)', color: 'var(--text-primary)' }}>
              <Icon size={10} /> {m === 'pvp' ? '2P' : m === 'easy' ? 'Easy' : 'Hard'}
            </button>
          ))}
        </div>
        <button onClick={resetBoard} className="p-1 rounded" style={{ background: 'var(--bg-input)', color: 'var(--accent-silver)' }}><RotateCcw size={14} /></button>
      </div>

      <div className="flex items-center justify-center gap-4 mb-3 text-xs">
        <span style={{ color: turn === 'X' ? 'var(--accent-silver)' : 'var(--text-muted)' }}>X: {scores.X}</span>
        <span style={{ color: 'var(--text-muted)' }}>Draw: {scores.draw}</span>
        <span style={{ color: turn === 'O' ? 'var(--accent-silver)' : 'var(--text-muted)' }}>O: {scores.O}</span>
      </div>

      <div className="flex-1 flex items-center justify-center">
        <div className="grid grid-cols-3 gap-2" style={{ width: 'min(100%, 280px)', aspectRatio: '1' }}>
          {board.map((cell, i) => {
            const isWinning = winner?.line?.includes(i);
            return (
              <button key={i} onClick={() => handleClick(i)}
                className="rounded-xl text-4xl font-bold flex items-center justify-center transition-all hover:scale-[1.02] active:scale-95"
                style={{
                  background: isWinning ? 'rgba(125,139,150,0.3)' : 'var(--bg-window)',
                  border: isWinning ? '2px solid var(--accent-silver)' : '1px solid rgba(0,0,0,0.06)',
                  color: cell === 'X' ? 'var(--accent-silver)' : cell === 'O' ? 'var(--error)' : 'var(--text-primary)',
                  boxShadow: isWinning ? '0 0 12px rgba(125,139,150,0.3)' : 'none',
                }}>
                {cell}
              </button>
            );
          })}
        </div>
      </div>

      {winner && (
        <div className="text-center mt-3">
          <span className="text-sm font-medium" style={{ color: winner.winner ? (winner.winner === 'X' ? 'var(--accent-silver)' : 'var(--error)') : 'var(--text-muted)' }}>
            {winner.winner ? `${winner.winner} wins!` : "It's a draw!"}
          </span>
        </div>
      )}

      {mode !== 'pvp' && turn === 'O' && !winner && (
        <div className="text-center mt-1 text-xs" style={{ color: 'var(--text-muted)' }}>AI thinking...</div>
      )}
    </div>
  );
}
