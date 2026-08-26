import { useState, useEffect } from 'react';
import { LayoutGrid, Wifi, Volume2, Battery, Bell, Search } from 'lucide-react';
import { useSystemStore } from '@/stores/useSystemStore';
import { useWindowStore } from '@/stores/useWindowStore';
import { useSettingsStore } from '@/stores/useSettingsStore';

interface TopPanelProps {
  onOpenAppMenu: () => void;
}

export default function TopPanel({ onOpenAppMenu }: TopPanelProps) {
  const currentTime = useSystemStore((s) => s.currentTime);
  const notifications = useSystemStore((s) => s.notifications);
  const windows = useWindowStore((s) => s.windows);
  const [showCalendar, setShowCalendar] = useState(false);
  const [showNotifs, setShowNotifs] = useState(false);
  const [showWifi, setShowWifi] = useState(false);
  const [showVol, setShowVol] = useState(false);
  const timeFormat = useSettingsStore((s) => s.timeFormat);
  const wifiNetworks = useSettingsStore((s) => s.wifiNetworks);
  const wifiEnabled = useSettingsStore((s) => s.wifiEnabled);
  const connectedWifi = useSettingsStore((s) => s.connectedWifi);
  const connectWifi = useSettingsStore((s) => s.connectWifi);
  const disconnectWifi = useSettingsStore((s) => s.disconnectWifi);
  const outputVolume = useSettingsStore((s) => s.outputVolume);
  const setOutputVolume = useSettingsStore((s) => s.setOutputVolume);
  const muted = useSettingsStore((s) => s.muted);
  const setMuted = useSettingsStore((s) => s.setMuted);

  const focusedWindow = windows.filter((w) => w.isFocused && !w.isMinimized)[0];
  const unreadCount = notifications.filter((n) => !n.read).length;

  const timeStr = timeFormat === '24h'
    ? currentTime.toLocaleTimeString('en-US', { hour: '2-digit', minute: '2-digit', hour12: false })
    : currentTime.toLocaleTimeString('en-US', { hour: 'numeric', minute: '2-digit', hour12: true });

  // Close popovers on outside click
  useEffect(() => {
    const handler = () => { setShowCalendar(false); setShowNotifs(false); setShowWifi(false); setShowVol(false); };
    if (showCalendar || showNotifs || showWifi || showVol) {
      const t = setTimeout(() => window.addEventListener('click', handler, { once: true }), 10);
      return () => { clearTimeout(t); window.removeEventListener('click', handler); };
    }
  }, [showCalendar, showNotifs, showWifi, showVol]);

  const year = currentTime.getFullYear();
  const month = currentTime.getMonth();
  const day = currentTime.getDate();

  // Simple calendar grid
  const firstDay = new Date(year, month, 1).getDay();
  const daysInMonth = new Date(year, month + 1, 0).getDate();
  const calendarDays: (number | null)[] = [...Array.from({ length: firstDay }, () => null), ...Array.from({ length: daysInMonth }, (_, i) => i + 1)];

  return (
    <div
      className="fixed top-0 left-0 right-0 h-9 flex items-center justify-between px-3 select-none z-40"
      style={{ background: 'rgba(15,15,35,0.9)', backdropFilter: 'blur(12px)', borderBottom: '1px solid rgba(255,255,255,0.08)' }}
    >
      {/* Left */}
      <div className="flex items-center gap-2">
        <button
          onClick={onOpenAppMenu}
          className="flex items-center gap-2 px-3 py-1 rounded text-white hover:bg-white/10 transition-colors"
        >
          <LayoutGrid size={16} />
          <span className="text-[13px] font-medium">Activities</span>
        </button>
      </div>

      {/* Center */}
      <div className="absolute left-1/2 -translate-x-1/2">
        <span className="text-[13px] font-medium text-white truncate max-w-[400px] block">
          {focusedWindow ? focusedWindow.title : 'WebOS Desktop'}
        </span>
      </div>

      {/* Right */}
      <div className="flex items-center gap-1">
        {/* WiFi */}
        <button onClick={(e) => { e.stopPropagation(); setShowWifi(!showWifi); }} className="relative p-1.5 rounded hover:bg-white/10 transition-colors">
          <Wifi size={16} className={connectedWifi ? 'text-white' : 'text-white/40'} />
        </button>
        {showWifi && (
          <div onClick={(e) => e.stopPropagation()} className="absolute top-10 right-36 w-64 rounded-lg p-3 z-50" style={{ background: 'rgba(30,30,45,0.98)', boxShadow: '0 8px 32px rgba(0,0,0,0.5)', border: '1px solid rgba(255,255,255,0.08)' }}>
            <div className="flex items-center justify-between mb-2">
              <span className="text-sm font-medium text-white">Wi-Fi</span>
              <button onClick={() => useSettingsStore.getState().setWifiEnabled(!wifiEnabled)}
                className={`w-10 h-5 rounded-full transition-colors ${wifiEnabled ? 'bg-[var(--accent-silver)]' : 'bg-white/20'}`}>
                <div className={`w-4 h-4 rounded-full bg-white transition-transform ${wifiEnabled ? 'translate-x-5' : 'translate-x-0.5'}`} />
              </button>
            </div>
            {wifiEnabled && wifiNetworks.map((net) => (
              <button key={net.ssid} onClick={() => net.connected ? disconnectWifi() : connectWifi(net.ssid)}
                className="w-full flex items-center justify-between py-1.5 px-2 rounded hover:bg-white/10 text-left">
                <span className="text-sm text-white">{net.ssid}</span>
                <div className="flex items-center gap-1">
                  {net.connected && <span className="text-[10px] text-green-400">Connected</span>}
                  <Wifi size={14} className="text-white/50" />
                </div>
              </button>
            ))}
          </div>
        )}

        {/* Volume */}
        <button onClick={(e) => { e.stopPropagation(); setShowVol(!showVol); }} className="p-1.5 rounded hover:bg-white/10 transition-colors">
          <Volume2 size={16} className={muted ? 'text-white/40' : 'text-white'} />
        </button>
        {showVol && (
          <div onClick={(e) => e.stopPropagation()} className="absolute top-10 right-20 w-10 p-2 rounded-lg z-50 flex flex-col items-center" style={{ background: 'rgba(30,30,30,0.98)', boxShadow: '0 8px 32px rgba(0,0,0,0.5)', border: '1px solid rgba(255,255,255,0.08)' }}>
            <span className="text-[10px] text-white/50 mb-1">{muted ? 0 : outputVolume}%</span>
            <input type="range" min={0} max={100} value={muted ? 0 : outputVolume}
              onChange={(e) => { setOutputVolume(Number(e.target.value)); if (Number(e.target.value) > 0) setMuted(false); }}
              className="w-24 -rotate-90 origin-center my-8 accent-[var(--accent-silver)]" />
          </div>
        )}

        {/* Battery */}
        <div className="flex items-center gap-1 px-1.5">
          <Battery size={16} className="text-green-400" />
        </div>

        {/* Notifications */}
        <button onClick={(e) => { e.stopPropagation(); setShowNotifs(!showNotifs); }} className="relative p-1.5 rounded hover:bg-white/10 transition-colors">
          <Bell size={16} className="text-white" />
          {unreadCount > 0 && (
            <span className="absolute top-0 right-0 w-2 h-2 rounded-full bg-[var(--error)]" />
          )}
        </button>
        {showNotifs && (
          <div onClick={(e) => e.stopPropagation()} className="absolute top-10 right-2 w-80 rounded-lg p-3 z-50 max-h-[70vh] overflow-y-auto" style={{ background: 'rgba(30,30,30,0.98)', boxShadow: '0 8px 32px rgba(0,0,0,0.5)', border: '1px solid rgba(255,255,255,0.08)' }}>
            <div className="flex items-center justify-between mb-3">
              <span className="text-sm font-semibold text-white">Notifications</span>
              <button onClick={() => useSystemStore.getState().clearNotifications()} className="text-xs text-white/50 hover:text-white">Clear all</button>
            </div>
            {notifications.length === 0 ? (
              <p className="text-sm text-white/50 text-center py-4">No notifications</p>
            ) : (
              notifications.map((n) => (
                <div key={n.id} onClick={() => useSystemStore.getState().markNotificationRead(n.id)}
                  className={`p-2 rounded mb-1 cursor-pointer hover:bg-white/10 ${n.read ? 'opacity-60' : ''}`}
                  style={{ borderLeft: `3px solid ${n.type === 'success' ? 'var(--success)' : n.type === 'error' ? 'var(--error)' : n.type === 'warning' ? 'var(--warning)' : 'var(--info)'}` }}>
                  <div className="text-sm font-medium text-white">{n.title}</div>
                  <div className="text-xs text-white/70">{n.message}</div>
                </div>
              ))
            )}
          </div>
        )}

        {/* Clock */}
        <button onClick={(e) => { e.stopPropagation(); setShowCalendar(!showCalendar); }} className="px-2 py-1 rounded hover:bg-white/10 transition-colors">
          <span className="font-mono text-[13px] text-white">{timeStr}</span>
        </button>
        {showCalendar && (
          <div onClick={(e) => e.stopPropagation()} className="absolute top-10 right-2 w-72 rounded-lg p-3 z-50" style={{ background: 'rgba(30,30,30,0.98)', boxShadow: '0 8px 32px rgba(0,0,0,0.5)', border: '1px solid rgba(255,255,255,0.08)' }}>
            <div className="text-center mb-2">
              <div className="text-lg font-medium text-white">{currentTime.toLocaleDateString('en-US', { weekday: 'long' })}</div>
              <div className="text-sm text-white/70">{currentTime.toLocaleDateString('en-US', { month: 'long', day: 'numeric', year: 'numeric' })}</div>
            </div>
            <div className="grid grid-cols-7 gap-1 text-center text-xs">
              {['Su','Mo','Tu','We','Th','Fr','Sa'].map((d) => <span key={d} className="text-white/50 font-medium">{d}</span>)}
              {calendarDays.map((d, i) => (
                <span key={i} className={`py-1 rounded ${d === day ? 'bg-[var(--accent-silver)] text-white' : d ? 'text-white hover:bg-white/10' : ''}`}>
                  {d || ''}
                </span>
              ))}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
