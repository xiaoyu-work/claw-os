import { User, LogOut } from 'lucide-react';
import { useSystemStore } from '@/stores/useSystemStore';
import { useSettingsStore } from '@/stores/useSettingsStore';

export default function UsersTab() {
  const { user } = useSystemStore();
  const { username, setUsername } = useSettingsStore();

  return (
    <div className="max-w-xl space-y-6">
      <h2 className="text-lg font-semibold mb-4">Users</h2>

      <div className="flex items-center gap-4 p-4 rounded-lg" style={{ background: 'var(--bg-window)' }}>
        <img src={user.avatar} alt="User" className="w-16 h-16 rounded-full object-cover" style={{ border: '2px solid var(--accent-silver)' }} />
        <div>
          <div className="text-base font-medium">{username}</div>
          <div className="text-xs text-[var(--text-muted)]">Administrator</div>
        </div>
      </div>

      <div className="p-4 rounded-lg space-y-3" style={{ background: 'var(--bg-window)' }}>
        <div>
          <label className="text-xs text-[var(--text-muted)] mb-1 block">Username</label>
          <input
            type="text"
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            className="w-full h-9 px-3 rounded text-sm outline-none"
            style={{ background: 'var(--bg-input)', color: 'var(--text-primary)', border: '1px solid rgba(0,0,0,0.06)' }}
          />
        </div>
        <div>
          <label className="text-xs text-[var(--text-muted)] mb-1 block">Change Password</label>
          <input
            type="password"
            placeholder="New password"
            className="w-full h-9 px-3 rounded text-sm outline-none mb-2"
            style={{ background: 'var(--bg-input)', color: 'var(--text-primary)', border: '1px solid rgba(0,0,0,0.06)' }}
          />
          <input
            type="password"
            placeholder="Confirm password"
            className="w-full h-9 px-3 rounded text-sm outline-none"
            style={{ background: 'var(--bg-input)', color: 'var(--text-primary)', border: '1px solid rgba(0,0,0,0.06)' }}
          />
        </div>
      </div>

      <button
        onClick={() => useSystemStore.getState().logout()}
        className="w-full py-2.5 rounded-lg text-sm font-medium flex items-center justify-center gap-2 transition-colors hover:opacity-90"
        style={{ background: 'var(--error)', color: 'white' }}
      >
        <LogOut size={16} />
        Log Out
      </button>
    </div>
  );
}
