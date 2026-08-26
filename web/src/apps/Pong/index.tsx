import { useState, useEffect, useRef, useCallback } from 'react';
import { RotateCcw, Play, Pause } from 'lucide-react';

const CANVAS_W = 600;
const CANVAS_H = 400;
const PADDLE_W = 12;
const PADDLE_H = 80;
const BALL_R = 8;
const WIN_SCORE = 10;

interface Ball { x: number; y: number; dx: number; dy: number; speed: number }
interface Paddle { y: number }

export default function Pong({ windowId: _windowId }: { windowId: string }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [playerScore, setPlayerScore] = useState(0);
  const [aiScore, setAiScore] = useState(0);
  const [gameOver, setGameOver] = useState(false);
  const [paused, setPaused] = useState(false);
  const [started, setStarted] = useState(false);
  const animRef = useRef<number>(0);
  const stateRef = useRef({
    ball: { x: CANVAS_W / 2, y: CANVAS_H / 2, dx: 4, dy: 3, speed: 5 } as Ball,
    player: { y: CANVAS_H / 2 - PADDLE_H / 2 } as Paddle,
    ai: { y: CANVAS_H / 2 - PADDLE_H / 2 } as Paddle,
    pScore: 0,
    aScore: 0,
    mouseY: CANVAS_H / 2,
  });

  const resetBall = useCallback(() => {
    const s = stateRef.current;
    s.ball = { x: CANVAS_W / 2, y: CANVAS_H / 2, dx: (Math.random() > 0.5 ? 1 : -1) * (3 + Math.random() * 2), dy: (Math.random() > 0.5 ? 1 : -1) * (2 + Math.random() * 2), speed: 5 + Math.min(s.pScore + s.aScore, 5) };
  }, []);

  const startGame = useCallback(() => {
    setStarted(true);
    setGameOver(false);
    setPaused(false);
    setPlayerScore(0);
    setAiScore(0);
    const s = stateRef.current;
    s.pScore = 0;
    s.aScore = 0;
    s.player.y = CANVAS_H / 2 - PADDLE_H / 2;
    s.ai.y = CANVAS_H / 2 - PADDLE_H / 2;
    resetBall();
  }, [resetBall]);

  useEffect(() => {
    if (!started || paused || gameOver) return;
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;

    const loop = () => {
      const s = stateRef.current;
      // Update player paddle
      s.player.y = s.mouseY - PADDLE_H / 2;
      s.player.y = Math.max(0, Math.min(CANVAS_H - PADDLE_H, s.player.y));

      // Update AI paddle
      const aiCenter = s.ai.y + PADDLE_H / 2;
      const target = s.ball.y;
      if (aiCenter < target - 10) s.ai.y += 3.5;
      else if (aiCenter > target + 10) s.ai.y -= 3.5;
      s.ai.y = Math.max(0, Math.min(CANVAS_H - PADDLE_H, s.ai.y));

      // Update ball
      s.ball.x += s.ball.dx * (s.ball.speed / 5);
      s.ball.y += s.ball.dy * (s.ball.speed / 5);

      // Wall collisions
      if (s.ball.y - BALL_R < 0 || s.ball.y + BALL_R > CANVAS_H) s.ball.dy *= -1;

      // Paddle collisions
      if (s.ball.x - BALL_R < PADDLE_W + 8 && s.ball.y > s.player.y && s.ball.y < s.player.y + PADDLE_H && s.ball.dx < 0) {
        s.ball.dx *= -1;
        const rel = (s.ball.y - (s.player.y + PADDLE_H / 2)) / (PADDLE_H / 2);
        s.ball.dy = rel * 4;
        s.ball.speed = Math.min(8, s.ball.speed + 0.3);
      }
      if (s.ball.x + BALL_R > CANVAS_W - PADDLE_W - 8 && s.ball.y > s.ai.y && s.ball.y < s.ai.y + PADDLE_H && s.ball.dx > 0) {
        s.ball.dx *= -1;
        const rel = (s.ball.y - (s.ai.y + PADDLE_H / 2)) / (PADDLE_H / 2);
        s.ball.dy = rel * 4;
        s.ball.speed = Math.min(8, s.ball.speed + 0.3);
      }

      // Score
      if (s.ball.x < 0) { s.aScore++; setAiScore(s.aScore); resetBall(); }
      if (s.ball.x > CANVAS_W) { s.pScore++; setPlayerScore(s.pScore); resetBall(); }

      if (s.pScore >= WIN_SCORE || s.aScore >= WIN_SCORE) { setGameOver(true); return; }

      // Draw
      ctx.fillStyle = '#F2F2F2';
      ctx.fillRect(0, 0, CANVAS_W, CANVAS_H);

      // Center line
      ctx.setLineDash([8, 8]);
      ctx.strokeStyle = 'rgba(0,0,0,0.10)';
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.moveTo(CANVAS_W / 2, 0);
      ctx.lineTo(CANVAS_W / 2, CANVAS_H);
      ctx.stroke();
      ctx.setLineDash([]);

      // Paddles
      ctx.fillStyle = '#7D8B96';
      ctx.shadowColor = 'rgba(125,139,150,0.4)';
      ctx.shadowBlur = 10;
      ctx.fillRect(8, s.player.y, PADDLE_W, PADDLE_H);
      ctx.fillStyle = '#EF4444';
      ctx.shadowColor = 'rgba(239,68,68,0.4)';
      ctx.fillRect(CANVAS_W - PADDLE_W - 8, s.ai.y, PADDLE_W, PADDLE_H);
      ctx.shadowBlur = 0;

      // Ball
      ctx.fillStyle = '#E2E8F0';
      ctx.shadowColor = 'rgba(226,232,240,0.4)';
      ctx.shadowBlur = 8;
      ctx.beginPath();
      ctx.arc(s.ball.x, s.ball.y, BALL_R, 0, Math.PI * 2);
      ctx.fill();
      ctx.shadowBlur = 0;

      // Scores
      ctx.font = 'bold 32px Inter';
      ctx.fillStyle = '#7D8B96';
      ctx.textAlign = 'center';
      ctx.fillText(String(s.pScore), CANVAS_W / 2 - 40, 40);
      ctx.fillStyle = '#EF4444';
      ctx.fillText(String(s.aScore), CANVAS_W / 2 + 40, 40);

      animRef.current = requestAnimationFrame(loop);
    };

    animRef.current = requestAnimationFrame(loop);
    return () => cancelAnimationFrame(animRef.current);
  }, [started, paused, gameOver, resetBall]);

  const handleMouseMove = useCallback((e: React.MouseEvent<HTMLCanvasElement>) => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const rect = canvas.getBoundingClientRect();
    const scaleY = CANVAS_H / rect.height;
    stateRef.current.mouseY = (e.clientY - rect.top) * scaleY;
  }, []);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'ArrowUp') stateRef.current.mouseY -= 20;
      if (e.key === 'ArrowDown') stateRef.current.mouseY += 20;
      if (e.key === ' ' || e.key === 'p') { e.preventDefault(); setPaused(p => !p); }
      stateRef.current.mouseY = Math.max(0, Math.min(CANVAS_H, stateRef.current.mouseY));
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, []);

  return (
    <div className="w-full h-full flex flex-col p-3 select-none" style={{ background: 'var(--bg-workspace)' }}>
      <div className="flex items-center justify-between mb-2">
        <div className="flex items-center gap-4 text-xs">
          <span style={{ color: 'var(--accent-silver)' }}>Player: <strong>{playerScore}</strong></span>
          <span style={{ color: 'var(--text-muted)' }}>vs</span>
          <span style={{ color: 'var(--error)' }}>AI: <strong>{aiScore}</strong></span>
        </div>
        <div className="flex gap-1">
          {started && <button onClick={() => setPaused(p => !p)} className="p-1 rounded" style={{ background: 'var(--bg-input)', color: 'var(--accent-silver)' }}>{paused ? <Play size={14} /> : <Pause size={14} />}</button>}
          <button onClick={startGame} className="p-1 rounded" style={{ background: 'var(--bg-input)', color: 'var(--accent-silver)' }}><RotateCcw size={14} /></button>
        </div>
      </div>

      <div className="flex-1 flex items-center justify-center">
        <canvas ref={canvasRef} width={CANVAS_W} height={CANVAS_H} onMouseMove={handleMouseMove}
          className="rounded-lg cursor-none max-w-full" style={{ border: '1px solid rgba(0,0,0,0.06)', maxHeight: '70vh', aspectRatio: '3/2' }} />
      </div>

      {!started && (
        <div className="absolute inset-0 z-50 flex items-center justify-center bg-black/60 rounded-lg">
          <div className="p-5 rounded-xl text-center" style={{ background: 'var(--bg-window)' }}>
            <h2 className="text-lg font-bold mb-2" style={{ color: 'var(--accent-silver)' }}>Pong</h2>
            <p className="text-xs mb-3" style={{ color: 'var(--text-muted)' }}>Mouse or Arrow keys to move<br/>First to {WIN_SCORE} wins</p>
            <button onClick={startGame} className="px-4 py-2 rounded-lg text-sm font-medium text-white" style={{ background: 'var(--accent-dark-gray)' }}>Start</button>
          </div>
        </div>
      )}

      {gameOver && (
        <div className="absolute inset-0 z-50 flex items-center justify-center bg-black/60 rounded-lg">
          <div className="p-5 rounded-xl text-center" style={{ background: 'var(--bg-window)' }}>
            <h2 className="text-lg font-bold mb-2" style={{ color: playerScore > aiScore ? 'var(--success)' : 'var(--error)' }}>
              {playerScore > aiScore ? 'You Win!' : 'AI Wins!'}
            </h2>
            <p className="text-xs mb-3" style={{ color: 'var(--text-muted)' }}>{playerScore} - {aiScore}</p>
            <button onClick={startGame} className="px-4 py-2 rounded-lg text-sm font-medium text-white" style={{ background: 'var(--accent-dark-gray)' }}>Play Again</button>
          </div>
        </div>
      )}

      {paused && started && !gameOver && (
        <div className="absolute inset-0 z-50 flex items-center justify-center bg-black/60 rounded-lg">
          <div className="p-4 rounded-xl text-center" style={{ background: 'var(--bg-window)' }}>
            <h2 className="text-base font-bold mb-2" style={{ color: 'var(--accent-silver)' }}>Paused</h2>
            <button onClick={() => setPaused(false)} className="px-4 py-2 rounded-lg text-sm font-medium text-white" style={{ background: 'var(--accent-dark-gray)' }}>Resume</button>
          </div>
        </div>
      )}
    </div>
  );
}
