import { useState, useCallback, useEffect } from 'react';
import { RotateCcw, Clock, Move } from 'lucide-react';

const SUITS = ['\u2665', '\u2666', '\u2663', '\u2660'] as const;
const RANKS = ['A', '2', '3', '4', '5', '6', '7', '8', '9', '10', 'J', 'Q', 'K'] as const;
type Suit = typeof SUITS[number];
type Rank = typeof RANKS[number];
interface Card { suit: Suit; rank: Rank; faceUp: boolean; id: number }

function createDeck(): Card[] {
  let id = 0;
  const cards: Card[] = [];
  for (const suit of SUITS) for (const rank of RANKS) cards.push({ suit, rank, faceUp: false, id: id++ });
  for (let i = cards.length - 1; i > 0; i--) { const j = Math.floor(Math.random() * (i + 1)); [cards[i], cards[j]] = [cards[j], cards[i]]; }
  return cards;
}

const isRed = (suit: Suit) => suit === '\u2665' || suit === '\u2666';
const rankValue = (rank: Rank) => RANKS.indexOf(rank);
const canPlaceOnTableau = (bottom: Card | null, top: Card) => {
  if (!bottom) return top.rank === 'K';
  return rankValue(top.rank) === rankValue(bottom.rank) - 1 && isRed(top.suit) !== isRed(bottom.suit);
};
const canPlaceOnFoundation = (top: Card | null, card: Card) => {
  if (!top) return card.rank === 'A';
  return card.suit === top.suit && rankValue(card.rank) === rankValue(top.rank) + 1;
};

export default function Solitaire({ windowId: _windowId }: { windowId: string }) {
  const [stock, setStock] = useState<Card[]>([]);
  const [waste, setWaste] = useState<Card[]>([]);
  const [foundations, setFoundations] = useState<Card[][]>([[], [], [], []]);
  const [tableau, setTableau] = useState<Card[][]>(Array.from({ length: 7 }, () => []));
  const [selected, setSelected] = useState<{ pile: 'tableau' | 'foundation' | 'waste'; index: number; cardIndex: number } | null>(null);
  const [moves, setMoves] = useState(0);
  const [timer, setTimer] = useState(0);
  const [won, setWon] = useState(false);

  useEffect(() => {
    if (won) return;
    const interval = setInterval(() => setTimer(t => t + 1), 1000);
    return () => clearInterval(interval);
  }, [won]);

  const deal = useCallback(() => {
    const deck = createDeck();
    const newTableau: Card[][] = Array.from({ length: 7 }, () => []);
    let idx = 0;
    for (let col = 0; col < 7; col++) {
      for (let row = 0; row <= col; row++) {
        newTableau[col].push({ ...deck[idx++], faceUp: row === col });
      }
    }
    setStock(deck.slice(idx));
    setWaste([]);
    setFoundations([[], [], [], []]);
    setTableau(newTableau);
    setSelected(null);
    setMoves(0);
    setTimer(0);
    setWon(false);
  }, []);

  useEffect(() => { deal(); }, [deal]);

  const handleStockClick = useCallback(() => {
    if (stock.length === 0) {
      if (waste.length > 0) { setStock(waste.slice().reverse().map(c => ({ ...c, faceUp: false }))); setWaste([]); }
      return;
    }
    const newStock = [...stock];
    const card = newStock.pop()!;
    setWaste(prev => [...prev, { ...card, faceUp: true }]);
    setStock(newStock);
  }, [stock, waste]);

  const handleCardClick = useCallback((pile: 'tableau' | 'foundation' | 'waste', index: number, cardIndex: number) => {
    if (won) return;
    const source = pile === 'tableau' ? tableau[index] : pile === 'foundation' ? foundations[index] : waste;
    const card = source[cardIndex];
    if (!card || (pile === 'tableau' && !card.faceUp)) {
      if (pile === 'tableau') {
        const col = tableau[index];
        const lastFaceDown = col.filter(c => !c.faceUp).length - 1;
        if (lastFaceDown >= 0) {
          const newTableau = tableau.map(c => [...c]);
          newTableau[index][lastFaceDown].faceUp = true;
          setTableau(newTableau);
        }
      }
      return;
    }

    if (!selected) {
      if (pile === 'foundation') {
        for (let f = 0; f < 4; f++) {
          if (f === index) continue;
          if (canPlaceOnFoundation(foundations[f].length > 0 ? foundations[f][foundations[f].length - 1] : null, card)) {
            const newFoundations = foundations.map(c => [...c]);
            const moved = newFoundations[index].pop()!;
            newFoundations[f].push(moved);
            setFoundations(newFoundations);
            setMoves(m => m + 1);
            return;
          }
        }
      }
      setSelected({ pile, index, cardIndex });
      return;
    }

    if (selected.pile === pile && selected.index === index) { setSelected(null); return; }

    const selSource = selected.pile === 'tableau' ? tableau[selected.index] : selected.pile === 'foundation' ? foundations[selected.index] : waste;
    const selCard = selSource[selected.cardIndex];
    if (!selCard) { setSelected(null); return; }

    if (pile === 'foundation') {
      if (selected.cardIndex === selSource.length - 1 && canPlaceOnFoundation(card, selCard)) {
        const newFoundations = foundations.map(c => [...c]);
        const newSource = selected.pile === 'tableau' ? tableau.map(c => [...c]) : selected.pile === 'foundation' ? newFoundations : null;
        const selArr = selected.pile === 'tableau' ? (newSource as Card[][])[selected.index] : selected.pile === 'foundation' ? newFoundations[selected.index] : [...waste];
        const moved = selArr.splice(selected.cardIndex, selSource.length - selected.cardIndex);
        newFoundations[index].push(...moved);
        if (selected.pile === 'tableau') setTableau(newSource as Card[][]);
        if (selected.pile === 'foundation') setFoundations(newFoundations);
        if (selected.pile === 'waste') setWaste(selArr);
        setFoundations(newFoundations);
        setMoves(m => m + 1);
        if (newFoundations.every(f => f.length === 13)) setWon(true);
      }
    } else if (pile === 'tableau') {
      const targetCol = tableau[index];
      const topCard = targetCol.length > 0 ? targetCol[targetCol.length - 1] : null;
      if (canPlaceOnTableau(topCard, selCard)) {
        const newTableau = tableau.map(c => [...c]);
        let moved: Card[];
        if (selected.pile === 'tableau') {
          moved = newTableau[selected.index].splice(selected.cardIndex, newTableau[selected.index].length - selected.cardIndex);
        } else if (selected.pile === 'foundation') {
          const newFoundations = foundations.map(c => [...c]);
          moved = [newFoundations[selected.index].pop()!];
          setFoundations(newFoundations);
        } else {
          moved = [waste.pop()!];
          setWaste([...waste]);
        }
        newTableau[index].push(...moved);
        if (selected.pile === 'tableau') setTableau(newTableau);
        else { setTableau(newTableau); }
        setMoves(m => m + 1);
      }
    }
    setSelected(null);
  }, [tableau, foundations, waste, selected, won]);

  const formatTime = (s: number) => `${Math.floor(s / 60)}:${(s % 60).toString().padStart(2, '0')}`;

  return (
    <div className="w-full h-full flex flex-col p-2 select-none overflow-auto" style={{ background: 'var(--bg-workspace)' }}>
      <div className="flex items-center justify-between mb-2">
        <div className="flex items-center gap-2 text-xs" style={{ color: 'var(--text-secondary)' }}>
          <span className="flex items-center gap-1"><Clock size={10} /> {formatTime(timer)}</span>
          <span className="flex items-center gap-1"><Move size={10} /> {moves}</span>
        </div>
        <button onClick={deal} className="p-1 rounded" style={{ background: 'var(--bg-input)', color: 'var(--accent-silver)' }}><RotateCcw size={14} /></button>
      </div>

      <div className="flex gap-2 mb-2">
        <button onClick={handleStockClick} className="w-10 h-14 rounded border border-dashed flex items-center justify-center text-xs" style={{ borderColor: 'var(--text-muted)', background: stock.length > 0 ? 'var(--bg-input)' : 'transparent', color: 'var(--text-muted)' }}>
          {stock.length}
        </button>
        <div className="w-10 h-14 rounded flex items-center justify-center" style={{ background: 'var(--bg-input)' }}>
          {waste.length > 0 && (
            <button onClick={() => handleCardClick('waste', 0, waste.length - 1)} className={`w-full h-full rounded flex flex-col items-center justify-center text-xs font-bold ${selected?.pile === 'waste' ? 'ring-2 ring-[var(--accent-silver)]' : ''}`} style={{ color: isRed(waste[waste.length - 1].suit) ? 'var(--error)' : 'var(--text-primary)', background: 'var(--bg-window)' }}>
              <span>{waste[waste.length - 1].rank}</span><span>{waste[waste.length - 1].suit}</span>
            </button>
          )}
        </div>
        <div className="flex-1" />
        {foundations.map((f, i) => (
          <button key={i} onClick={() => handleCardClick('foundation', i, f.length - 1)} className={`w-10 h-14 rounded flex items-center justify-center text-lg ${selected?.pile === 'foundation' && selected?.index === i ? 'ring-2 ring-[var(--accent-silver)]' : ''}`} style={{ background: 'var(--bg-input)' }}>
            {f.length > 0 ? <span style={{ color: isRed(f[f.length - 1].suit) ? 'var(--error)' : 'var(--text-primary)' }}>{f[f.length - 1].suit}</span> : <span style={{ color: 'var(--text-muted)' }}>{SUITS[i]}</span>}
          </button>
        ))}
      </div>

      <div className="flex gap-1 flex-1">
        {tableau.map((col, ci) => (
          <div key={ci} className="flex-1 flex flex-col gap-0.5 min-w-0">
            {col.map((card, ri) => (
              <button key={card.id}
                onClick={() => handleCardClick('tableau', ci, ri)}
                className={`w-full h-8 rounded text-[10px] font-bold flex items-center justify-center transition-all ${selected?.pile === 'tableau' && selected?.index === ci && ri >= selected.cardIndex ? 'ring-1 ring-[var(--accent-silver)]' : ''}`}
                style={{
                  background: card.faceUp ? 'var(--bg-window)' : 'var(--bg-input)',
                  color: card.faceUp ? (isRed(card.suit) ? 'var(--error)' : 'var(--text-primary)') : 'transparent',
                  marginTop: ri === 0 ? 0 : -12,
                }}
              >
                {card.faceUp ? <><span>{card.rank}</span><span className="ml-0.5">{card.suit}</span></> : ''}
              </button>
            ))}
          </div>
        ))}
      </div>

      {won && (
        <div className="absolute inset-0 z-50 flex items-center justify-center bg-black/60 rounded-lg">
          <div className="p-5 rounded-xl text-center" style={{ background: 'var(--bg-window)' }}>
            <h2 className="text-lg font-bold mb-2" style={{ color: 'var(--success)' }}>You Win!</h2>
            <p className="text-xs mb-1" style={{ color: 'var(--text-muted)' }}>Time: {formatTime(timer)} | Moves: {moves}</p>
            <button onClick={deal} className="px-4 py-2 rounded-lg text-sm font-medium text-white mt-2" style={{ background: 'var(--accent-dark-gray)' }}>New Game</button>
          </div>
        </div>
      )}
    </div>
  );
}
