import { Moon, Palette, Sun, Volume1 } from 'lucide-react';
import ScriptedAssistantPanel, {
  type ScriptedAssistantAction,
  type ScriptedAssistantResponse,
} from '@/components/ScriptedAssistantPanel';
import { publicAsset } from '@/lib/publicAsset';
import { useSettingsStore } from '@/stores/useSettingsStore';

interface SettingsAiPanelProps {
  open: boolean;
  onClose: () => void;
  onShowTab: (tab: string) => void;
}

const actions: ScriptedAssistantAction[] = [
  {
    id: 'light',
    label: 'Light & bright',
    detail: 'Light theme · 95%',
    icon: Sun,
    prompt: 'Make the desktop light and bright',
  },
  {
    id: 'focus',
    label: 'Dark focus',
    detail: 'Dark · dimmer · quiet',
    icon: Moon,
    prompt: 'Turn on dark focus mode',
  },
  {
    id: 'accent',
    label: 'Blue accent',
    detail: 'Use Claw OS blue',
    icon: Palette,
    prompt: 'Use a blue accent color',
  },
  {
    id: 'quiet',
    label: 'Quiet mode',
    detail: 'Output volume · 25%',
    icon: Volume1,
    prompt: 'Set a quiet 25% output volume',
  },
];

export default function SettingsAiPanel({
  open,
  onClose,
  onShowTab,
}: SettingsAiPanelProps) {
  const settings = useSettingsStore();

  const applyPrompt = (prompt: string): ScriptedAssistantResponse<never> => {
    const normalized = prompt.toLowerCase();
    const changes: string[] = [];
    let tab = 'appearance';

    if (normalized.includes('light')) {
      settings.setTheme('light');
      changes.push('switched to the light theme');
    } else if (normalized.includes('dark') || normalized.includes('focus')) {
      settings.setTheme('dark');
      changes.push('switched to the dark theme');
    }

    if (normalized.includes('blue') || normalized.includes('蓝')) {
      settings.setAccentColor('#005CFE');
      changes.push('set the accent to Claw OS blue');
    } else if (normalized.includes('purple') || normalized.includes('紫')) {
      settings.setAccentColor('#7C3AED');
      changes.push('set the accent to purple');
    } else if (normalized.includes('gold') || normalized.includes('金')) {
      settings.setAccentColor('#B89A60');
      changes.push('set the accent to gold');
    }

    if (normalized.includes('frost')) {
      settings.setWallpaper(publicAsset('wallpaper-frost.jpg'));
      changes.push('selected Frost Glass wallpaper');
    } else if (normalized.includes('marble')) {
      settings.setWallpaper(publicAsset('wallpaper-marble.jpg'));
      changes.push('selected Light Marble wallpaper');
    } else if (normalized.includes('concrete') || normalized.includes('white wallpaper')) {
      settings.setWallpaper(publicAsset('wallpaper-concrete.jpg'));
      changes.push('selected White Concrete wallpaper');
    }

    if (normalized.includes('bright')) {
      settings.setBrightness(95);
      changes.push('set brightness to 95%');
      tab = 'display';
    } else if (normalized.includes('dimmer') || normalized.includes('dim ')) {
      settings.setBrightness(55);
      changes.push('set brightness to 55%');
      tab = 'display';
    }

    const volumeMatch = normalized.match(/(\d{1,3})\s*%/);
    if ((normalized.includes('volume') || normalized.includes('quiet')) && volumeMatch) {
      const volume = Math.min(100, Number(volumeMatch[1]));
      settings.setOutputVolume(volume);
      settings.setMuted(false);
      changes.push(`set output volume to ${volume}%`);
      tab = 'sound';
    } else if (normalized.includes('quiet')) {
      settings.setOutputVolume(25);
      settings.setMuted(false);
      changes.push('set output volume to 25%');
      tab = 'sound';
    }

    if (normalized.includes('unmute')) {
      settings.setMuted(false);
      changes.push('unmuted audio');
      tab = 'sound';
    } else if (normalized.includes('mute')) {
      settings.setMuted(true);
      changes.push('muted audio');
      tab = 'sound';
    }

    if (normalized.includes('wifi off') || normalized.includes('disable wifi')) {
      settings.setWifiEnabled(false);
      changes.push('turned Wi-Fi off');
      tab = 'network';
    } else if (normalized.includes('wifi on') || normalized.includes('enable wifi')) {
      settings.setWifiEnabled(true);
      changes.push('turned Wi-Fi on');
      tab = 'network';
    }

    if (normalized.includes('focus')) {
      settings.setBrightness(60);
      settings.setOutputVolume(30);
      settings.setMuted(false);
      changes.push('reduced brightness to 60% and volume to 30%');
      tab = 'display';
    }

    if (changes.length === 0) {
      return {
        text: 'Try asking for light or dark mode, a blue or purple accent, a wallpaper, brightness, volume, mute, or Wi-Fi. This UI demo applies supported settings immediately.',
      };
    }

    onShowTab(tab);
    return { text: `Done — I ${changes.join(', ')}.` };
  };

  return (
    <ScriptedAssistantPanel<never>
      panelId="settings"
      open={open}
      title="Settings Assistant"
      subtitle="Describe a change and apply it"
      initialMessage="Tell me how you want the desktop to look or behave. Supported demo settings are applied immediately with scripted local actions."
      actions={actions}
      placeholder="What would you like to change?"
      answer={applyPrompt}
      onClose={onClose}
    />
  );
}
