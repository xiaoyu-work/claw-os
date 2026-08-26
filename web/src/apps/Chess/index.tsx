import { useState, useCallback, useEffect } from 'react';
import { RotateCcw, Undo2 } from 'lucide-react';

type PieceType = 'p' | 'r' | 'n' | 'b' | 'q' | 'k';
type PieceColor = 'w' | 'b';
interface Piece { type: PieceType; color: PieceColor }
type Board = (Piece | null)[][];
interface Move { from: [number, number]; to: [number, number]; piece: Piece; captured?: Piece; promotion?: PieceType; enPassant?: boolean; castle?: 'kingside' | 'queenside' }

const PIECE_SYMBOLS: Record<PieceColor, Record<PieceType, string>> = {
  w: { k: '\u2654', q: '\u2655', r: '\u2656', b: '\u2657', n: '\u2658', p: '\u2659' },
  b: { k: '\u265A', q: '\u265B', r: '\u265C', b: '\u265D', n: '\u265E', p: '\u265F' },
};

const initialBoard = (): Board => {
  const board: Board = Array.from({ length: 8 }, () => Array(8).fill(null));
  const backRow: PieceType[] = ['r', 'n', 'b', 'q', 'k', 'b', 'n', 'r'];
  for (let c = 0; c < 8; c++) {
    board[0][c] = { type: backRow[c], color: 'b' };
    board[1][c] = { type: 'p', color: 'b' };
    board[6][c] = { type: 'p', color: 'w' };
    board[7][c] = { type: backRow[c], color: 'w' };
  }
  return board;
};

const inBounds = (r: number, c: number) => r >= 0 && r < 8 && c >= 0 && c < 8;

const getMoves = (board: Board, row: number, col: number, enPassantTarget: [number, number] | null): [number, number][] => {
  const piece = board[row][col];
  if (!piece) return [];
  const moves: [number, number][] = [];
  const { type, color } = piece;
  const dir = color === 'w' ? -1 : 1;

  if (type === 'p') {
    const startRow = color === 'w' ? 6 : 1;
    if (inBounds(row + dir, col) && !board[row + dir][col]) {
      moves.push([row + dir, col]);
      if (row === startRow && !board[row + 2 * dir][col]) moves.push([row + 2 * dir, col]);
    }
    for (const dc of [-1, 1]) {
      const nr = row + dir, nc = col + dc;
      if (inBounds(nr, nc)) {
        if (board[nr][nc] && board[nr][nc]!.color !== color) moves.push([nr, nc]);
        if (enPassantTarget && enPassantTarget[0] === nr && enPassantTarget[1] === nc) moves.push([nr, nc]);
      }
    }
  }
  if (type === 'r' || type === 'q') {
    for (const [dr, dc] of [[-1, 0], [1, 0], [0, -1], [0, 1]]) {
      for (let i = 1; i < 8; i++) {
        const nr = row + dr * i, nc = col + dc * i;
        if (!inBounds(nr, nc)) break;
        if (!board[nr][nc]) { moves.push([nr, nc]); continue; }
        if (board[nr][nc]!.color !== color) moves.push([nr, nc]);
        break;
      }
    }
  }
  if (type === 'b' || type === 'q') {
    for (const [dr, dc] of [[-1, -1], [-1, 1], [1, -1], [1, 1]]) {
      for (let i = 1; i < 8; i++) {
        const nr = row + dr * i, nc = col + dc * i;
        if (!inBounds(nr, nc)) break;
        if (!board[nr][nc]) { moves.push([nr, nc]); continue; }
        if (board[nr][nc]!.color !== color) moves.push([nr, nc]);
        break;
      }
    }
  }
  if (type === 'n') {
    for (const [dr, dc] of [[-2, -1], [-2, 1], [-1, -2], [-1, 2], [1, -2], [1, 2], [2, -1], [2, 1]]) {
      const nr = row + dr, nc = col + dc;
      if (inBounds(nr, nc) && (!board[nr][nc] || board[nr][nc]!.color !== color)) moves.push([nr, nc]);
    }
  }
  if (type === 'k') {
    for (const [dr, dc] of [[-1, -1], [-1, 0], [-1, 1], [0, -1], [0, 1], [1, -1], [1, 0], [1, 1]]) {
      const nr = row + dr, nc = col + dc;
      if (inBounds(nr, nc) && (!board[nr][nc] || board[nr][nc]!.color !== color)) moves.push([nr, nc]);
    }
  }
  return moves;
};

const isSquareAttacked = (board: Board, row: number, col: number, byColor: PieceColor): boolean => {
  for (let r = 0; r < 8; r++) {
    for (let c = 0; c < 8; c++) {
      const p = board[r][c];
      if (p && p.color === byColor) {
        const mvs = getMoves(board, r, c, null);
        if (mvs.some(([mr, mc]) => mr === row && mc === col)) return true;
      }
    }
  }
  return false;
};

const findKing = (board: Board, color: PieceColor): [number, number] | null => {
  for (let r = 0; r < 8; r++) for (let c = 0; c < 8; c++) if (board[r][c]?.type === 'k' && board[r][c]?.color === color) return [r, c];
  return null;
};

const isInCheck = (board: Board, color: PieceColor): boolean => {
  const king = findKing(board, color);
  if (!king) return false;
  return isSquareAttacked(board, king[0], king[1], color === 'w' ? 'b' : 'w');
};

const getLegalMoves = (board: Board, row: number, col: number, enPassantTarget: [number, number] | null, castleRights: Record<PieceColor, { king: boolean; queen: boolean }>): [number, number][] => {
  const piece = board[row][col];
  if (!piece) return [];
  const moves = getMoves(board, row, col, enPassantTarget);
  const legal: [number, number][] = [];
  for (const [tr, tc] of moves) {
    const newBoard = board.map(r => [...r]);
    newBoard[tr][tc] = piece;
    newBoard[row][col] = null;
    if (piece.type === 'p' && enPassantTarget && tr === enPassantTarget[0] && tc === enPassantTarget[1]) {
      newBoard[row][tc] = null;
    }
    if (!isInCheck(newBoard, piece.color)) legal.push([tr, tc]);
  }
  if (piece.type === 'k') {
    const color = piece.color;
    if (castleRights[color].king && board[row][5] === null && board[row][6] === null && !isSquareAttacked(board, row, 4, color === 'w' ? 'b' : 'w') && !isSquareAttacked(board, row, 5, color === 'w' ? 'b' : 'w') && !isSquareAttacked(board, row, 6, color === 'w' ? 'b' : 'w')) {
      if (board[row][7]?.type === 'r' && board[row][7]?.color === color) legal.push([row, 6]);
    }
    if (castleRights[color].queen && board[row][3] === null && board[row][2] === null && board[row][1] === null && !isSquareAttacked(board, row, 4, color === 'w' ? 'b' : 'w') && !isSquareAttacked(board, row, 3, color === 'w' ? 'b' : 'w') && !isSquareAttacked(board, row, 2, color === 'w' ? 'b' : 'w')) {
      if (board[row][0]?.type === 'r' && board[row][0]?.color === color) legal.push([row, 2]);
    }
  }
  return legal;
};

const evaluateBoard = (board: Board): number => {
  const values: Record<PieceType, number> = { p: 1, n: 3, b: 3, r: 5, q: 9, k: 0 };
  let score = 0;
  for (let r = 0; r < 8; r++) for (let c = 0; c < 8; c++) {
    const p = board[r][c];
    if (p) score += (p.color === 'w' ? 1 : -1) * values[p.type];
  }
  return score;
};

const minimax = (board: Board, depth: number, alpha: number, beta: number, maximizing: boolean, enPassantTarget: [number, number] | null, castleRights: Record<PieceColor, { king: boolean; queen: boolean }>): number => {
  if (depth === 0) return evaluateBoard(board);
  const color: PieceColor = maximizing ? 'b' : 'w';
  let hasMove = false;
  for (let r = 0; r < 8; r++) {
    for (let c = 0; c < 8; c++) {
      const p = board[r][c];
      if (p && p.color === color) {
        const mvs = getLegalMoves(board, r, c, enPassantTarget, castleRights);
        for (const [mr, mc] of mvs) {
          hasMove = true;
          const newBoard = board.map(row => [...row]);
          newBoard[mr][mc] = p;
          newBoard[r][c] = null;
          const ep = p.type === 'p' && Math.abs(mr - r) === 2 ? [(r + mr) / 2, c] as [number, number] : null;
          const val = minimax(newBoard, depth - 1, alpha, beta, !maximizing, ep, castleRights);
          if (maximizing) alpha = Math.max(alpha, val); else beta = Math.min(beta, val);
          if (beta <= alpha) return maximizing ? alpha : beta;
        }
      }
    }
  }
  if (!hasMove) {
    if (isInCheck(board, color)) return maximizing ? -999 : 999;
    return 0;
  }
  return maximizing ? alpha : beta;
};

const getAIMove = (board: Board, enPassantTarget: [number, number] | null, castleRights: Record<PieceColor, { king: boolean; queen: boolean }>): Move | null => {
  let bestMove: Move | null = null;
  let bestVal = -Infinity;
  const moves: Move[] = [];
  for (let r = 0; r < 8; r++) {
    for (let c = 0; c < 8; c++) {
      const p = board[r][c];
      if (p && p.color === 'b') {
        const mvs = getLegalMoves(board, r, c, enPassantTarget, castleRights);
        for (const [mr, mc] of mvs) {
          const captured = board[mr][mc] || undefined;
          moves.push({ from: [r, c], to: [mr, mc], piece: p, captured });
        }
      }
    }
  }
  if (moves.length === 0) return null;
  for (const move of moves) {
    const newBoard = board.map(row => [...row]);
    newBoard[move.to[0]][move.to[1]] = move.piece;
    newBoard[move.from[0]][move.from[1]] = null;
    const ep = move.piece.type === 'p' && Math.abs(move.to[0] - move.from[0]) === 2 ? [(move.from[0] + move.to[0]) / 2, move.from[1]] as [number, number] : null;
    const val = minimax(newBoard, 1, -Infinity, Infinity, false, ep, castleRights);
    if (val > bestVal) { bestVal = val; bestMove = move; }
  }
  return bestMove;
};

export default function Chess({ windowId: _windowId }: { windowId: string }) {
  const [board, setBoard] = useState<Board>(initialBoard);
  const [turn, setTurn] = useState<PieceColor>('w');
  const [selected, setSelected] = useState<[number, number] | null>(null);
  const [legalMoves, setLegalMoves] = useState<[number, number][]>([]);
  const [history, setHistory] = useState<Move[]>([]);
  const [capturedByWhite, setCapturedByWhite] = useState<Piece[]>([]);
  const [capturedByBlack, setCapturedByBlack] = useState<Piece[]>([]);
  const [enPassantTarget, setEnPassantTarget] = useState<[number, number] | null>(null);
  const [castleRights, setCastleRights] = useState<Record<PieceColor, { king: boolean; queen: boolean }>>({ w: { king: true, queen: true }, b: { king: true, queen: true } });
  const [promotionSquare, setPromotionSquare] = useState<[number, number] | null>(null);
  const [promotionFromTo, setPromotionFromTo] = useState<[[number, number], [number, number]] | null>(null);
  const [gameOver, setGameOver] = useState<string | null>(null);
  const [moveIndex, setMoveIndex] = useState(-1);

  const checkGameEnd = useCallback((brd: Board, color: PieceColor, ep: [number, number] | null, cr: Record<PieceColor, { king: boolean; queen: boolean }>) => {
    let hasMove = false;
    for (let r = 0; r < 8; r++) {
      for (let c = 0; c < 8; c++) {
        const p = brd[r][c];
        if (p && p.color === color) {
          if (getLegalMoves(brd, r, c, ep, cr).length > 0) { hasMove = true; break; }
        }
      }
      if (hasMove) break;
    }
    if (!hasMove) {
      if (isInCheck(brd, color)) setGameOver(color === 'w' ? 'Black wins by checkmate!' : 'White wins by checkmate!');
      else setGameOver('Stalemate - Draw!');
    }
  }, []);

  const handleSquareClick = useCallback((row: number, col: number) => {
    if (gameOver || turn !== 'w') return;
    const piece = board[row][col];
    if (selected) {
      const isLegal = legalMoves.some(([r, c]) => r === row && c === col);
      if (isLegal) {
        const movingPiece = board[selected[0]][selected[1]]!;
        const captured = board[row][col] || undefined;
        if (movingPiece.type === 'p' && (row === 0 || row === 7)) {
          setPromotionSquare([row, col]);
          setPromotionFromTo([selected, [row, col]]);
          setSelected(null);
          setLegalMoves([]);
          return;
        }
        executeMove(selected, [row, col], movingPiece, captured);
      } else {
        setSelected(null);
        setLegalMoves([]);
        if (piece && piece.color === 'w') {
          setSelected([row, col]);
          setLegalMoves(getLegalMoves(board, row, col, enPassantTarget, castleRights));
        }
      }
    } else if (piece && piece.color === 'w') {
      setSelected([row, col]);
      setLegalMoves(getLegalMoves(board, row, col, enPassantTarget, castleRights));
    }
  }, [board, selected, legalMoves, turn, gameOver, enPassantTarget, castleRights]);

  const executeMove = (from: [number, number], to: [number, number], piece: Piece, captured?: Piece, promoteTo?: PieceType) => {
    const newBoard = board.map(r => [...r]);
    newBoard[to[0]][to[1]] = promoteTo ? { type: promoteTo, color: piece.color } : piece;
    newBoard[from[0]][from[1]] = null;
    let ep = false;
    if (piece.type === 'p' && enPassantTarget && to[0] === enPassantTarget[0] && to[1] === enPassantTarget[1]) {
      newBoard[from[0]][to[1]] = null;
      ep = true;
    }
    let castle: 'kingside' | 'queenside' | undefined;
    if (piece.type === 'k' && to[1] - from[1] === 2) { newBoard[to[0]][5] = newBoard[to[0]][7]; newBoard[to[0]][7] = null; castle = 'kingside'; }
    if (piece.type === 'k' && to[1] - from[1] === -2) { newBoard[to[0]][3] = newBoard[to[0]][0]; newBoard[to[0]][0] = null; castle = 'queenside'; }
    const move: Move = { from, to, piece, captured: captured || (ep ? { type: 'p', color: piece.color === 'w' ? 'b' : 'w' } : undefined), promotion: promoteTo, enPassant: ep, castle };
    if (captured) {
      if (piece.color === 'w') setCapturedByWhite(prev => [...prev, captured]); else setCapturedByBlack(prev => [...prev, captured]);
    }
    if (ep && piece.color === 'w') setCapturedByWhite(prev => [...prev, { type: 'p', color: 'b' }]);
    if (ep && piece.color === 'b') setCapturedByBlack(prev => [...prev, { type: 'p', color: 'w' }]);
    setBoard(newBoard);
    setHistory(prev => [...prev, move]);
    setMoveIndex(prev => prev + 1);
    const newEP = piece.type === 'p' && Math.abs(to[0] - from[0]) === 2 ? [(from[0] + to[0]) / 2, from[1]] as [number, number] : null;
    setEnPassantTarget(newEP);
    const newCR = { ...castleRights };
    if (piece.type === 'k') { newCR[piece.color] = { king: false, queen: false }; }
    if (piece.type === 'r') {
      if (from[1] === 7) newCR[piece.color] = { ...newCR[piece.color], king: false };
      if (from[1] === 0) newCR[piece.color] = { ...newCR[piece.color], queen: false };
    }
    setCastleRights(newCR);
    setSelected(null);
    setLegalMoves([]);
    setTurn('b');
    checkGameEnd(newBoard, 'b', newEP, newCR);
  };

  const handlePromotion = (type: PieceType) => {
    if (!promotionFromTo) return;
    const [from, to] = promotionFromTo;
    const piece = board[from[0]][from[1]]!;
    const captured = board[to[0]][to[1]] || undefined;
    executeMove(from, to, piece, captured, type);
    setPromotionSquare(null);
    setPromotionFromTo(null);
  };

  useEffect(() => {
    if (turn === 'b' && !gameOver) {
      const timer = setTimeout(() => {
        const move = getAIMove(board, enPassantTarget, castleRights);
        if (move) {
          let promote: PieceType | undefined;
          if (move.piece.type === 'p' && (move.to[0] === 0 || move.to[0] === 7)) {
            promote = 'q';
          }
          const newBoard = board.map(r => [...r]);
          newBoard[move.to[0]][move.to[1]] = promote ? { type: promote, color: 'b' } : move.piece;
          newBoard[move.from[0]][move.from[1]] = null;
          let ep = false;
          if (move.piece.type === 'p' && enPassantTarget && move.to[0] === enPassantTarget[0] && move.to[1] === enPassantTarget[1]) {
            newBoard[move.from[0]][move.to[1]] = null; ep = true;
          }
          let castle: 'kingside' | 'queenside' | undefined;
          if (move.piece.type === 'k' && move.to[1] - move.from[1] === 2) { newBoard[move.to[0]][5] = newBoard[move.to[0]][7]; newBoard[move.to[0]][7] = null; castle = 'kingside'; }
          if (move.piece.type === 'k' && move.to[1] - move.from[1] === -2) { newBoard[move.to[0]][3] = newBoard[move.to[0]][0]; newBoard[move.to[0]][0] = null; castle = 'queenside'; }
          if (move.captured) setCapturedByBlack(prev => [...prev, move.captured!]);
          if (ep) setCapturedByBlack(prev => [...prev, { type: 'p', color: 'w' }]);
          setBoard(newBoard);
          const fullMove: Move = { ...move, enPassant: ep, castle, promotion: promote };
          setHistory(prev => [...prev, fullMove]);
          setMoveIndex(prev => prev + 1);
          const newEP = move.piece.type === 'p' && Math.abs(move.to[0] - move.from[0]) === 2 ? [(move.from[0] + move.to[0]) / 2, move.from[1]] as [number, number] : null;
          setEnPassantTarget(newEP);
          const newCR = { ...castleRights };
          if (move.piece.type === 'k') newCR.b = { king: false, queen: false };
          if (move.piece.type === 'r') {
            if (move.from[1] === 7) newCR.b = { ...newCR.b, king: false };
            if (move.from[1] === 0) newCR.b = { ...newCR.b, queen: false };
          }
          setCastleRights(newCR);
          setTurn('w');
          checkGameEnd(newBoard, 'w', newEP, newCR);
        }
      }, 500);
      return () => clearTimeout(timer);
    }
  }, [turn, board, enPassantTarget, castleRights, gameOver]);

  const resetGame = () => {
    setBoard(initialBoard()); setTurn('w'); setSelected(null); setLegalMoves([]);
    setHistory([]); setCapturedByWhite([]); setCapturedByBlack([]);
    setEnPassantTarget(null); setCastleRights({ w: { king: true, queen: true }, b: { king: true, queen: true } });
    setPromotionSquare(null); setPromotionFromTo(null); setGameOver(null); setMoveIndex(-1);
  };

  const undoMove = () => {
    if (moveIndex < 0 || history.length === 0) return;
    resetGame();
  };

  const capturedValue = (pieces: Piece[]) => pieces.reduce((sum, p) => sum + ({ p: 1, n: 3, b: 3, r: 5, q: 9, k: 0 }[p.type] || 0), 0);

  return (
    <div className="w-full h-full flex flex-col p-3 select-none" style={{ background: 'var(--bg-workspace)' }}>
      {gameOver && (
        <div className="absolute inset-0 z-50 flex items-center justify-center bg-black/60 rounded-lg">
          <div className="p-6 rounded-xl text-center" style={{ background: 'var(--bg-window)' }}>
            <h2 className="text-xl font-bold mb-3" style={{ color: 'var(--accent-silver)' }}>{gameOver}</h2>
            <button onClick={resetGame} className="px-4 py-2 rounded-lg text-sm font-medium text-white" style={{ background: 'var(--accent-dark-gray)' }}>New Game</button>
          </div>
        </div>
      )}
      {promotionSquare && (
        <div className="absolute inset-0 z-50 flex items-center justify-center bg-black/60 rounded-lg">
          <div className="p-4 rounded-xl flex gap-2" style={{ background: 'var(--bg-window)' }}>
            {(['q', 'r', 'b', 'n'] as PieceType[]).map(t => (
              <button key={t} onClick={() => handlePromotion(t)} className="w-12 h-12 text-2xl flex items-center justify-center rounded-lg hover:bg-[var(--bg-hover)]" style={{ background: 'var(--bg-input)' }}>
                {PIECE_SYMBOLS.w[t]}
              </button>
            ))}
          </div>
        </div>
      )}
      <div className="flex items-center justify-between mb-2">
        <div className="flex items-center gap-2">
          <button onClick={resetGame} className="p-1.5 rounded-lg hover:bg-[var(--bg-hover)]" title="New Game"><RotateCcw size={14} /></button>
          <button onClick={undoMove} className="p-1.5 rounded-lg hover:bg-[var(--bg-hover)]" title="Reset"><Undo2 size={14} /></button>
        </div>
        <div className="text-xs font-medium" style={{ color: turn === 'w' ? '#fff' : '#94A3B8' }}>{turn === 'w' ? 'Your turn (White)' : 'AI thinking...'}</div>
        <div className="flex items-center gap-1 text-xs" style={{ color: 'var(--text-muted)' }}>
          <span>+{capturedValue(capturedByWhite)}</span>
          <span className="mx-1">|</span>
          <span>+{capturedValue(capturedByBlack)}</span>
        </div>
      </div>
      <div className="flex flex-1 gap-3 min-h-0">
        <div className="flex-1 flex flex-col items-center justify-center">
          <div className="grid grid-cols-8 gap-0.5 rounded-lg overflow-hidden" style={{ width: 'min(100%, 400px)', aspectRatio: '1' }}>
            {Array.from({ length: 8 }, (_, row) =>
              Array.from({ length: 8 }, (_, col) => {
                const isLight = (row + col) % 2 === 0;
                const isSelected = selected?.[0] === row && selected?.[1] === col;
                const isLegal = legalMoves.some(([r, c]) => r === row && c === col);
                const piece = board[row][col];
                const kingPos = findKing(board, 'w');
                const inCheck = kingPos?.[0] === row && kingPos?.[1] === col && isInCheck(board, 'w');
                return (
                  <button
                    key={`${row}-${col}`}
                    onClick={() => handleSquareClick(row, col)}
                    className="relative flex items-center justify-center text-2xl sm:text-3xl transition-all"
                    style={{
                      background: isSelected ? 'rgba(125,139,150,0.5)' : inCheck ? 'rgba(239,68,68,0.4)' : isLight ? '#E8D5B5' : '#B58863',
                    }}
                  >
                    {piece && <span style={{ color: piece.color === 'w' ? '#fff' : '#1a1a2e', textShadow: piece.color === 'w' ? '0 1px 2px rgba(0,0,0,0.08)' : 'none', filter: piece.color === 'b' ? 'drop-shadow(0 1px 1px rgba(0,0,0,0.3))' : 'none' }}>{PIECE_SYMBOLS[piece.color][piece.type]}</span>}
                    {isLegal && !piece && <div className="w-3 h-3 rounded-full bg-[rgba(76,29,149,0.5)]" />}
                    {isLegal && piece && <div className="absolute inset-0 rounded-full border-2 border-[rgba(76,29,149,0.4)]" />}
                  </button>
                );
              })
            )}
          </div>
        </div>
        <div className="w-40 flex flex-col gap-2" style={{ minWidth: 160 }}>
          <div className="p-2 rounded-lg flex-1 overflow-hidden flex flex-col" style={{ background: 'var(--bg-window)' }}>
            <div className="text-xs font-medium mb-1" style={{ color: 'var(--accent-silver)' }}>Move History</div>
            <div className="flex-1 overflow-y-auto text-[10px]" style={{ color: 'var(--text-muted)' }}>
              {history.length === 0 && <span className="italic">No moves yet</span>}
              {history.map((m, i) => (
                <div key={i} className="py-0.5">{i + 1}. {m.piece.color === 'w' ? 'W' : 'B'}{m.piece.type.toUpperCase()} {String.fromCharCode(97 + m.from[1])}{8 - m.from[0]}→{String.fromCharCode(97 + m.to[1])}{8 - m.to[0]}</div>
              ))}
            </div>
          </div>
          <div className="p-2 rounded-lg" style={{ background: 'var(--bg-window)' }}>
            <div className="text-xs font-medium mb-1" style={{ color: 'var(--accent-silver)' }}>Captured</div>
            <div className="flex flex-wrap gap-0.5 text-xs mb-1">{capturedByWhite.map((p, i) => <span key={i}>{PIECE_SYMBOLS['b'][p.type]}</span>)}</div>
            <div className="flex flex-wrap gap-0.5 text-xs">{capturedByBlack.map((p, i) => <span key={i}>{PIECE_SYMBOLS['w'][p.type]}</span>)}</div>
          </div>
        </div>
      </div>
    </div>
  );
}
