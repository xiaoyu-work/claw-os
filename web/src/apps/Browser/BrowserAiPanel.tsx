import { BookOpen, HelpCircle, Info, Rocket } from 'lucide-react';
import ScriptedAssistantPanel, {
  type ScriptedAssistantAction,
  type ScriptedAssistantResponse,
} from '@/components/ScriptedAssistantPanel';

interface BrowserAiPanelProps {
  open: boolean;
  onClose: () => void;
  onNavigateSection: (section: string) => void;
}

const actions: ScriptedAssistantAction[] = [
  {
    id: 'summarize',
    label: 'Summarize page',
    detail: 'Key Claw OS ideas',
    icon: BookOpen,
    prompt: 'Summarize this page',
  },
  {
    id: 'agent-native',
    label: 'Explain agent-native',
    detail: 'Why the OS is different',
    icon: Info,
    prompt: 'Explain what agent-native means',
  },
  {
    id: 'install',
    label: 'Install options',
    detail: 'Jump to installation',
    icon: Rocket,
    prompt: 'Show me how to install Claw OS',
  },
  {
    id: 'faq',
    label: 'Open FAQ',
    detail: 'Common questions',
    icon: HelpCircle,
    prompt: 'Take me to the FAQ',
  },
];

export default function BrowserAiPanel({
  open,
  onClose,
  onNavigateSection,
}: BrowserAiPanelProps) {
  const answer = (prompt: string): ScriptedAssistantResponse<never> => {
    const normalized = prompt.toLowerCase();
    if (normalized.includes('install') || normalized.includes('安装')) {
      onNavigateSection('install');
      return {
        text: 'I opened the installation section. It shows the WSL and Docker paths plus the first Agent setup command.',
      };
    }
    if (normalized.includes('faq') || normalized.includes('question') || normalized.includes('问题')) {
      onNavigateSection('faq');
      return {
        text: 'I opened the FAQ, where the page explains agent-native design, supported models, app integration, and safety.',
      };
    }
    if (normalized.includes('demo') || normalized.includes('try') || normalized.includes('体验')) {
      onNavigateSection('demo');
      return {
        text: 'I opened the interactive Agent demo so you can try system health, crash analysis, workflows, models, memory, and app access.',
      };
    }
    if (normalized.includes('safe') || normalized.includes('permission') || normalized.includes('安全')) {
      onNavigateSection('safety');
      return {
        text: 'Claw OS gates privileged operations with explicit capabilities, approvals, structured tool evidence, audit history, and undo where supported.',
      };
    }
    if (normalized.includes('agent-native') || normalized.includes('agent native') || normalized.includes('智能体')) {
      onNavigateSection('why');
      return {
        text: 'Agent-native means the Agent is a system layer, not a chatbot inside one app. It can understand the machine, use approved app operations, share the OS model runtime, and compose workflows under capability controls.',
      };
    }
    if (normalized.includes('summar') || normalized.includes('总结') || normalized.includes('page')) {
      return {
        text: 'This page presents Claw OS as an agent-native Linux distribution: one built-in system Agent, one shared AI runtime for apps, typed app operations for workflows, and approval plus audit controls.',
      };
    }
    return {
      text: 'Ask me to summarize the page, explain agent-native design or safety, open the interactive demo, find installation options, or jump to the FAQ.',
    };
  };

  return (
    <ScriptedAssistantPanel<never>
      panelId="browser"
      open={open}
      title="Browser Assistant"
      subtitle="Understand and navigate this page"
      initialMessage="Ask about the current Claw OS page or use a built-in prompt. Navigation and responses are local scripted demo actions."
      actions={actions}
      placeholder="Ask about this page…"
      answer={answer}
      onClose={onClose}
    />
  );
}
