import { useState, useEffect } from 'react';
import { LogIn, Power, RefreshCw, Users } from 'lucide-react';
import { useSystemStore } from '@/stores/useSystemStore';
import { useSettingsStore } from '@/stores/useSettingsStore';

export default function LoginScreen() {
  const [password, setPassword] = useState('');
  const [error, setError] = useState('');
  const [isLoggingIn, setIsLoggingIn] = useState(false);
  const [currentTime, setCurrentTime] = useState(new Date());
  const login = useSystemStore((s) => s.login);
  const user = useSystemStore((s) => s.user);
  const wallpaper = useSettingsStore((s) => s.wallpaper);
  const timeFormat = useSettingsStore((s) => s.timeFormat);

  useEffect(() => {
    const interval = setInterval(() => setCurrentTime(new Date()), 1000);
    return () => clearInterval(interval);
  }, []);

  const timeStr = timeFormat === '24h'
    ? currentTime.toLocaleTimeString('en-US', { hour: '2-digit', minute: '2-digit', hour12: false })
    : currentTime.toLocaleTimeString('en-US', { hour: 'numeric', minute: '2-digit', hour12: true });

  const dateStr = currentTime.toLocaleDateString('en-US', { weekday: 'long', month: 'long', day: 'numeric' });

  const handleLogin = () => {
    if (!password.trim()) { setError('Password is required'); return; }
    setIsLoggingIn(true);
    setError('');
    setTimeout(() => {
      if (login(user.username, password)) {
        setIsLoggingIn(false);
      } else {
        setIsLoggingIn(false);
        setError('Incorrect password');
      }
    }, 600);
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      handleLogin();
    }
  };

  return (
    <div className="fixed inset-0 z-[9998] flex flex-col items-center justify-center"
      style={{
        backgroundImage: `url(${wallpaper})`,
        backgroundSize: 'cover',
        backgroundPosition: 'center',
      }}>
      {/* Solid overlay - hides desktop underneath */}
      <div className="absolute inset-0" style={{ background: '#E8E8E8' }} />

      {/* Login content */}
      <div className="relative z-10 flex flex-col items-center">
        {/* Clock */}
        <div className="mb-2" style={{ transitionDelay: '100ms' }}>
          <span className="text-5xl font-light text-[var(--text-primary)]" style={{ letterSpacing: '-0.02em' }}>
            {timeStr}
          </span>
        </div>

        {/* Date */}
        <div className="mb-8">
          <span className="text-base text-[var(--text-secondary)]">{dateStr}</span>
        </div>

        {/* Avatar */}
        <div className="mb-3">
          <img
            src={user.avatar}
            alt="User"
            className="w-20 h-20 rounded-full object-cover"
            style={{ border: '2px solid rgba(125,139,150,0.3)', boxShadow: '0 0 20px rgba(125,139,150,0.1)' }}
          />
        </div>

        {/* Username */}
        <div className="mb-4">
          <span className="text-lg font-medium text-[var(--text-primary)]">{user.username}</span>
        </div>

        {/* Password */}
        <div className="flex flex-col items-center gap-2">
          <div className="relative">
            <input
              type="password"
              value={password}
              onChange={(e) => { setPassword(e.target.value); setError(''); }}
              onKeyDown={handleKeyDown}
              placeholder="Password"
              autoFocus
              className={`w-60 h-10 px-4 rounded text-sm text-[var(--text-primary)] outline-none transition-all ${
                error ? 'border-red-500' : ''
              }`}
              style={{
                background: 'var(--bg-input)',
                border: error ? '1px solid var(--error)' : '1px solid rgba(0,0,0,0.12)',
              }}
            />
          </div>
          {error && <span className="text-xs text-[var(--error)]">{error}</span>}
          <span className="text-xs text-[var(--text-muted)]">Password: 123456</span>

          <button
            onClick={handleLogin}
            disabled={isLoggingIn}
            className="w-60 h-10 rounded text-sm font-medium text-white flex items-center justify-center gap-2 transition-all hover:opacity-90 active:scale-[0.98] disabled:opacity-70"
            style={{ background: 'var(--accent-silver)' }}
          >
            {isLoggingIn ? (
              <div className="w-5 h-5 border-2 border-white/30 border-t-white rounded-full animate-spin" />
            ) : (
              <>
                <LogIn size={16} />
                Sign In
              </>
            )}
          </button>
        </div>

        {/* Bottom actions */}
        <div className="flex items-center gap-6 mt-6">
          <button className="flex items-center gap-1.5 text-[13px] text-[var(--text-secondary)] hover:text-[var(--text-primary)] transition-colors">
            <Users size={16} />
            Switch User
          </button>
          <button onClick={() => useSystemStore.getState().powerOff()}
            className="flex items-center gap-1.5 text-[13px] text-[var(--text-secondary)] hover:text-[var(--error)] transition-colors">
            <Power size={16} />
            Power Off
          </button>
          <button onClick={() => useSystemStore.getState().restart()}
            className="flex items-center gap-1.5 text-[13px] text-[var(--text-secondary)] hover:text-[var(--warning)] transition-colors">
            <RefreshCw size={16} />
            Restart
          </button>
        </div>
      </div>
    </div>
  );
}
