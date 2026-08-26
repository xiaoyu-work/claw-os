import { Wifi, WifiOff, Lock } from 'lucide-react';
import { useSettingsStore } from '@/stores/useSettingsStore';

export default function NetworkTab() {
  const { wifiEnabled, setWifiEnabled, wifiNetworks, connectedWifi, connectWifi, disconnectWifi, ipAddress } = useSettingsStore();

  return (
    <div className="max-w-xl space-y-6">
      <h2 className="text-lg font-semibold mb-4">Network</h2>

      <div className="flex items-center justify-between p-3 rounded-lg" style={{ background: 'var(--bg-window)' }}>
        <div className="flex items-center gap-3">
          {wifiEnabled ? <Wifi size={20} className="text-[var(--accent-silver)]" /> : <WifiOff size={20} className="text-[var(--text-muted)]" />}
          <div>
            <div className="text-sm font-medium">Wi-Fi</div>
            <div className="text-xs text-[var(--text-muted)]">{connectedWifi || 'Not connected'}</div>
          </div>
        </div>
        <button
          onClick={() => setWifiEnabled(!wifiEnabled)}
          className={`w-12 h-6 rounded-full transition-colors ${wifiEnabled ? 'bg-[var(--accent-dark-gray)]' : 'bg-[var(--bg-input)]'}`}
        >
          <div className={`w-5 h-5 rounded-full bg-white transition-transform mx-0.5 ${wifiEnabled ? 'translate-x-6' : ''}`} />
        </button>
      </div>

      {connectedWifi && (
        <div className="p-3 rounded-lg" style={{ background: 'var(--bg-window)' }}>
          <div className="text-xs text-[var(--text-muted)] mb-1">IP Address</div>
          <div className="text-sm font-mono">{ipAddress}</div>
        </div>
      )}

      {wifiEnabled && (
        <div>
          <label className="text-sm font-medium text-[var(--text-secondary)] mb-2 block">Available Networks</label>
          <div className="space-y-1">
            {wifiNetworks.map((net) => (
              <button
                key={net.ssid}
                onClick={() => net.connected ? disconnectWifi() : connectWifi(net.ssid)}
                className="w-full flex items-center justify-between p-3 rounded-lg hover:bg-[var(--bg-hover)] transition-colors text-left"
                style={{ background: 'var(--bg-window)' }}
              >
                <div className="flex items-center gap-3">
                  <Wifi size={16} className={net.connected ? 'text-[var(--accent-silver)]' : 'text-[var(--text-muted)]'} />
                  <div>
                    <div className="text-sm">{net.ssid}</div>
                    <div className="text-xs text-[var(--text-muted)]">{net.connected ? 'Connected' : `${net.strength}% signal`}</div>
                  </div>
                </div>
                <div className="flex items-center gap-2">
                  {net.secured && <Lock size={14} className="text-[var(--text-muted)]" />}
                  {net.connected && <span className="text-xs text-[var(--success)]">Connected</span>}
                </div>
              </button>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
