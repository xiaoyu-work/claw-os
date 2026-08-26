import { useState, useEffect, useCallback, useRef } from 'react';
import { RotateCcw, Pause, Play, Trophy } from 'lucide-react';

const GRID = 20;
const INITIAL_SPEED = 150;

interface Position { x: number; y: number }

function randPos(snake: Position[]): Position {
  let pos: Position;
  do {
    pos = { x: Math.floor(Math.random() * GRID), y: Math.floor(Math.random() * GRID) };
  } while (snake.some(s => s.x === pos.x && s.y === pos.y));
  return pos;
}

export default function Snake({ windowId: _windowId }: { windowId: string }) {
  const [snake, setSnake] = useState<Position[]>([{ x: 10, y: 10 }, { x: 9, y: 10 }, { x: 8, y: 10 }]);
  const [food, setFood] = useState<Position>({ x: 15, y: 10 });
  const [dir, setDir] = useState<Position>({ x: 1, y: 0 });
  const [score, setScore] = useState(0);
  const [highScore, setHighScore] = useState(() => parseInt(localStorage.getItem('snake_highscore') || '0'));
  const [gameOver, setGameOver] = useState(false);
  const [paused, setPaused] = useState(false);
  const dirRef = useRef(dir);
  dirRef.current = dir;
  const pausedRef = useRef(paused);
  pausedRef.current = paused;
  const gameOverRef = useRef(gameOver);
  gameOverRef.current = gameOver;

  const speed = Math.max(60, INITIAL_SPEED - Math.floor(score / 5) * 8);

  const reset = useCallback(() => {
    const startSnake = [{ x: 10, y: 10 }, { x: 9, y: 10 }, { x: 8, y: 10 }];
    setSnake(startSnake);
    setFood(randPos(startSnake));
    setDir({ x: 1, y: 0 });
    setScore(0);
    setGameOver(false);
    setPaused(false);
  }, []);

  useEffect(() => {
    if (gameOver || paused) return;
    const interval = setInterval(() => {
      setSnake(prev => {
        const newSnake = [...prev];
        const head = { ...newSnake[0] };
        head.x += dirRef.current.x;
        head.y += dirRef.current.y;
        if (head.x < 0 || head.x >= GRID || head.y < 0 || head.y >= GRID || newSnake.some(s => s.x === head.x && s.y === head.y)) {
          setGameOver(true);
          gameOverRef.current = true;
          return prev;
        }
        newSnake.unshift(head);
        const f = food;
        if (head.x === f.x && head.y === f.y) {
          setScore(s => {
            const newScore = s + 10;
            setHighScore(hs => {
              const nh = Math.max(hs, newScore);
              localStorage.setItem('snake_highscore', String(nh));
              return nh;
            });
            return newScore;
          });
          setFood(randPos(newSnake));
        } else {
          newSnake.pop();
        }
        return newSnake;
      });
    }, speed);
    return () => clearInterval(interval);
  }, [gameOver, paused, speed, food]);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (gameOverRef.current) return;
      const d = dirRef.current;
      if ((e.key === 'ArrowUp' || e.key === 'w' || e.key === 'W') && d.y === 0) setDir({ x: 0, y: -1 });
      if ((e.key === 'ArrowDown' || e.key === 's' || e.key === 'S') && d.y === 0) setDir({ x: 0, y: 1 });
      if ((e.key === 'ArrowLeft' || e.key === 'a' || e.key === 'A') && d.x === 0) setDir({ x: -1, y: 0 });
      if ((e.key === 'ArrowRight' || e.key === 'd' || e.key === 'D') && d.x === 0) setDir({ x: 1, y: 0 });
      if (e.key === ' ' || e.key === 'p' || e.key === 'P') { e.preventDefault(); setPaused(p => !p); }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, []);

  return (
    <div className="w-full h-full flex flex-col p-3 select-none" style={{ background: 'var(--bg-workspace)' }}>
      <div className="flex items-center justify-between mb-2">
        <div className="flex items-center gap-3 text-xs">
          <span style={{ color: 'var(--text-secondary)' }}>Score: <strong style={{ color: 'var(--accent-silver)' }}>{score}</strong></span>
          <span className="flex items-center gap-1" style={{ color: 'var(--text-muted)' }}><Trophy size={10} /> {highScore}</span>
          <span style={{ color: 'var(--text-secondary)' }}>Speed: <strong style={{ color: 'var(--accent-silver)' }}>{Math.floor((INITIAL_SPEED - speed) / 8) + 1}</strong></span>
        </div>
        <div className="flex gap-1">
          <button onClick={() => setPaused(p => !p)} className="p-1 rounded" style={{ background: 'var(--bg-input)', color: 'var(--accent-silver)' }}>{paused ? <Play size={14} /> : <Pause size={14} />}</button>
          <button onClick={reset} className="p-1 rounded" style={{ background: 'var(--bg-input)', color: 'var(--accent-silver)' }}><RotateCcw size={14} /></button>
        </div>
      </div>
      <div className="flex-1 flex items-center justify-center">
        <div className="grid gap-px rounded overflow-hidden border border-[rgba(0,0,0,0.12)]" style={{ gridTemplateColumns: `repeat(${GRID}, 1fr)`, background: 'rgba(0,0,0,0.06)' }}>
          {Array.from({ length: GRID * GRID }, (_, i) => {
            const x = i % GRID, y = Math.floor(i / GRID);
            const isSnake = snake.some(s => s.x === x && s.y === y);
            const isHead = snake[0]?.x === x && snake[0]?.y === y;
            const isFood = food.x === x && food.y === y;
            return (
              <div key={i} className="transition-colors"
                style={{
                  width: 'clamp(10px, 3.5vw, 18px)', height: 'clamp(10px, 3.5vw, 18px)',
                  background: isHead ? 'var(--accent-silver)' : isSnake ? 'rgba(125,139,150,0.6)' : isFood ? 'var(--error)' : 'rgba(30,30,46,0.8)',
                  borderRadius: isSnake ? 3 : 0,
                  boxShadow: isFood ? '0 0 6px rgba(239,68,68,0.5)' : isHead ? '0 0 6px rgba(125,139,150,0.5)' : 'none',
                }}
              />
            );
          })}
        </div>
      </div>
      {(gameOver || paused) && (
        <div className="absolute inset-0 z-50 flex items-center justify-center bg-black/60 rounded-lg">
          <div className="p-5 rounded-xl text-center" style={{ background: 'var(--bg-window)' }}>
            <h2 className="text-lg font-bold mb-2" style={{ color: gameOver ? 'var(--error)' : 'var(--accent-silver)' }}>{gameOver ? 'Game Over!' : 'Paused'}</h2>
            {gameOver && <p className="text-xs mb-3" style={{ color: 'var(--text-muted)' }}>Score: {score} {score === highScore && score > 0 ? '(New Best!)' : ''}</p>}
            <button onClick={gameOver ? reset : () => setPaused(false)} className="px-4 py-2 rounded-lg text-sm font-medium text-white" style={{ background: 'var(--accent-dark-gray)' }}>{gameOver ? 'New Game' : 'Resume'}</button>
          </div>
        </div>
      )}
    </div>
  );
}
