import { useState, useEffect, useCallback } from 'react';
import { RotateCcw, Clock, MousePointerClick } from 'lucide-react';

const EMOJIS = ['\u{1F34E}', '\u{1F34C}', '\u{1F347}', '\u{1F349}', '\u{1F353}', '\u{1F352}', '\u{1F34A}', '\u{1F348}'];

interface Card { emoji: string; flipped: boolean; matched: boolean; id: number }

function shuffle(): Card[] {
  const pairs = [...EMOJIS, ...EMOJIS];
  for (let i = pairs.length - 1; i > 0; i--) { const j = Math.floor(Math.random() * (i + 1)); [pairs[i], pairs[j]] = [pairs[j], pairs[i]]; }
  return pairs.map((emoji, id) => ({ emoji, flipped: false, matched: false, id }));
}

export default function MemoryMatch({ windowId: _windowId }: { windowId: string }) {
  const [cards, setCards] = useState<Card[]>(shuffle);
  const [flipped, setFlipped] = useState<number[]>([]);
  const [moves, setMoves] = useState(0);
  const [timer, setTimer] = useState(0);
  const [gameOver, setGameOver] = useState(false);
  const [lock, setLock] = useState(false);

  useEffect(() => {
    if (gameOver) return;
    const interval = setInterval(() => setTimer(t => t + 1), 1000);
    return () => clearInterval(interval);
  }, [gameOver]);

  const handleClick = useCallback((id: number) => {
    if (lock || gameOver) return;
    const card = cards.find(c => c.id === id);
    if (!card || card.flipped || card.matched) return;

    const newCards = cards.map(c => c.id === id ? { ...c, flipped: true } : c);
    setCards(newCards);
    const newFlipped = [...flipped, id];
    setFlipped(newFlipped);

    if (newFlipped.length === 2) {
      setLock(true);
      setMoves(m => m + 1);
      const [a, b] = newFlipped;
      const cardA = newCards.find(c => c.id === a)!;
      const cardB = newCards.find(c => c.id === b)!;
      if (cardA.emoji === cardB.emoji) {
        setTimeout(() => {
          const matched = newCards.map(c => (c.id === a || c.id === b) ? { ...c, matched: true } : c);
          setCards(matched);
          setFlipped([]);
          setLock(false);
          if (matched.every(c => c.matched)) setGameOver(true);
        }, 400);
      } else {
        setTimeout(() => {
          setCards(prev => prev.map(c => (c.id === a || c.id === b) ? { ...c, flipped: false } : c));
          setFlipped([]);
          setLock(false);
        }, 800);
      }
    }
  }, [cards, flipped, lock, gameOver]);

  const reset = () => {
    setCards(shuffle());
    setFlipped([]);
    setMoves(0);
    setTimer(0);
    setGameOver(false);
    setLock(false);
  };

  const formatTime = (s: number) => `${Math.floor(s / 60)}:${(s % 60).toString().padStart(2, '0')}`;

  return (
    <div className="w-full h-full flex flex-col p-3 select-none" style={{ background: 'var(--bg-workspace)' }}>
      <div className="flex items-center justify-between mb-3">
        <div className="flex items-center gap-3 text-xs" style={{ color: 'var(--text-secondary)' }}>
          <span className="flex items-center gap-1"><MousePointerClick size={10} /> {moves}</span>
          <span className="flex items-center gap-1"><Clock size={10} /> {formatTime(timer)}</span>
        </div>
        <button onClick={reset} className="p-1 rounded" style={{ background: 'var(--bg-input)', color: 'var(--accent-silver)' }}><RotateCcw size={14} /></button>
      </div>

      <div className="flex-1 flex items-center justify-center">
        <div className="grid grid-cols-4 gap-2" style={{ width: 'min(100%, 320px)' }}>
          {cards.map(card => (
            <button key={card.id} onClick={() => handleClick(card.id)}
              className="aspect-square rounded-xl flex items-center justify-center text-3xl transition-all"
              style={{
                background: card.matched ? 'rgba(34,197,94,0.15)' : card.flipped ? 'var(--bg-window)' : 'var(--bg-input)',
                border: card.matched ? '1px solid rgba(34,197,94,0.3)' : card.flipped ? '1px solid var(--accent-silver)' : '1px solid rgba(0,0,0,0.06)',
                transform: card.flipped || card.matched ? 'rotateY(0deg)' : 'rotateY(0deg)',
                boxShadow: card.flipped ? '0 0 8px rgba(125,139,150,0.15)' : 'none',
              }}>
              {(card.flipped || card.matched) ? card.emoji : '?'}
            </button>
          ))}
        </div>
      </div>

      {gameOver && (
        <div className="absolute inset-0 z-50 flex items-center justify-center bg-black/60 rounded-lg">
          <div className="p-5 rounded-xl text-center" style={{ background: 'var(--bg-window)' }}>
            <h2 className="text-lg font-bold mb-2" style={{ color: 'var(--success)' }}>You Win!</h2>
            <p className="text-xs mb-1" style={{ color: 'var(--text-muted)' }}>Moves: {moves} | Time: {formatTime(timer)}</p>
            <button onClick={reset} className="px-4 py-2 rounded-lg text-sm font-medium text-white mt-2" style={{ background: 'var(--accent-dark-gray)' }}>New Game</button>
          </div>
        </div>
      )}
    </div>
  );
}
