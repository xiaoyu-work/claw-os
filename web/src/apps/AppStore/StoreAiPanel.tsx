import { Bot, FileText, Palette, ShieldCheck } from 'lucide-react';
import AppIcon from '@/components/AppIcon';
import ScriptedAssistantPanel, {
  type ScriptedAssistantAction,
  type ScriptedAssistantResponse,
} from '@/components/ScriptedAssistantPanel';
import type { AppDefinition } from '@/stores/useAppRegistryStore';

interface StoreAiPanelProps {
  open: boolean;
  apps: AppDefinition[];
  onClose: () => void;
  onOpenApp: (app: AppDefinition) => void;
}

const actions: ScriptedAssistantAction[] = [
  {
    id: 'writing',
    label: 'Write & organize',
    detail: 'Documents and notes',
    icon: FileText,
    prompt: 'I need apps to write and organize project notes',
  },
  {
    id: 'media',
    label: 'Create media',
    detail: 'Audio, video, capture',
    icon: Palette,
    prompt: 'I want apps for media and screenshots',
  },
  {
    id: 'automation',
    label: 'Automate work',
    detail: 'Agent and workflows',
    icon: Bot,
    prompt: 'I want to automate repetitive work',
  },
  {
    id: 'privacy',
    label: 'Privacy & control',
    detail: 'Permissions and settings',
    icon: ShieldCheck,
    prompt: 'I need apps for privacy and permission control',
  },
];

function recommendApps(prompt: string, apps: AppDefinition[]): ScriptedAssistantResponse<AppDefinition> {
  const normalized = prompt.toLowerCase();
  let ids: string[];
  let text: string;

  if (
    normalized.includes('privacy')
    || normalized.includes('permission')
    || normalized.includes('security')
    || normalized.includes('隐私')
    || normalized.includes('权限')
  ) {
    ids = ['settings', 'agent'];
    text = 'Use Settings to manage system controls and Claw OS Agent to explain app permissions, activity, and audit history.';
  } else if (
    normalized.includes('media')
    || normalized.includes('music')
    || normalized.includes('video')
    || normalized.includes('photo')
    || normalized.includes('screenshot')
    || normalized.includes('媒体')
    || normalized.includes('截图')
  ) {
    ids = ['player', 'screenshot'];
    text = 'Media Player handles local audio and video, while Screenshot captures the screen, a window, or a selected area.';
  } else if (
    normalized.includes('write')
    || normalized.includes('document')
    || normalized.includes('note')
    || normalized.includes('file')
    || normalized.includes('写')
    || normalized.includes('文档')
    || normalized.includes('文件')
  ) {
    ids = ['texteditor', 'filemanager', 'agent'];
    text = 'Text Editor and Files cover writing and organization. Add Claw OS Agent when you want summaries or a cross-app workflow.';
  } else if (
    normalized.includes('automat')
    || normalized.includes('workflow')
    || normalized.includes('agent')
    || normalized.includes('ai')
    || normalized.includes('自动')
    || normalized.includes('工作流')
  ) {
    ids = ['agent', 'filemanager'];
    text = 'Start with Claw OS Agent for guided workflows, then use Files as the document and storage surface it can work with.';
  } else {
    const exact = apps.find((app) => normalized.includes(app.name.toLowerCase()));
    ids = exact ? [exact.id] : ['agent', 'filemanager', 'settings'];
    text = exact
      ? `${exact.name} is the closest match to your request.`
      : 'Tell me whether you want to write, automate, create media, or manage privacy. These are the best general starting points.';
  }

  const results = ids
    .map((id) => apps.find((app) => app.id === id))
    .filter((app): app is AppDefinition => Boolean(app));
  return { text, results };
}

export default function StoreAiPanel({
  open,
  apps,
  onClose,
  onOpenApp,
}: StoreAiPanelProps) {
  return (
    <ScriptedAssistantPanel
      panelId="store"
      open={open}
      title="App Finder"
      subtitle="Describe what you want to do"
      initialMessage="Tell me what kind of app you need. This preview matches your request to the installed Claw OS demo catalog using built-in rules."
      actions={actions}
      placeholder="What kind of app do you need?"
      answer={(prompt) => recommendApps(prompt, apps)}
      onClose={onClose}
      renderResult={(app) => (
        <div
          data-store-recommendation={app.id}
          className="flex items-center gap-2 rounded-lg border p-2"
          style={{ background: 'var(--bg-window)', borderColor: 'rgba(0,0,0,0.08)' }}
        >
          <AppIcon icon={app.icon} label={app.name} size={30} className="shrink-0" />
          <div className="min-w-0 flex-1">
            <div className="truncate text-[11px] font-medium text-[var(--text-primary)]">{app.name}</div>
            <div className="truncate text-[9px] text-[var(--text-muted)]">{app.description}</div>
          </div>
          <button
            type="button"
            onClick={() => onOpenApp(app)}
            className="shrink-0 rounded-lg bg-[#005CFE] px-2 py-1 text-[10px] font-medium text-white"
          >
            Open
          </button>
        </div>
      )}
    />
  );
}
