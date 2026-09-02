import {
  Activity,
  BrainCircuit,
  Bug,
  History,
  Network,
  ShieldCheck,
  type LucideIcon,
} from 'lucide-react';

export type RunPhase = 'approval' | 'running' | 'complete' | 'denied' | 'stopped';

export interface Scenario {
  id: string;
  title: string;
  subtitle: string;
  icon: LucideIcon;
  prompt: string;
  intro: string;
  plan: string[];
  scopes: Array<[string, string]>;
  tools: Array<[string, string]>;
  resultTitle: string;
  result: string;
  audit: string;
}

export interface MessageItem {
  id: number;
  kind: 'message';
  role: 'user' | 'assistant';
  text: string;
}

export interface RunItem {
  id: number;
  kind: 'run';
  scenarioId: string;
  phase: RunPhase;
  completedTools: number;
  auditOpen: boolean;
}

export type TimelineItem = MessageItem | RunItem;

export const scenarios: Scenario[] = [
  {
    id: 'health',
    title: 'System health',
    subtitle: 'Find why the network is slow',
    icon: Activity,
    prompt: 'Why is my network slow right now?',
    intro: 'I’ll correlate network activity, running apps, and recent service logs before I explain the cause.',
    plan: [
      'Measure current sockets, routes, DNS latency, and throughput.',
      'Find the apps and background jobs using the most bandwidth.',
      'Correlate NetworkManager logs with the activity timeline.',
    ],
    scopes: [
      ['sys.net.read', 'interfaces, routes, and sockets'],
      ['app.activity.read', 'network use by installed apps'],
      ['log.read', 'NetworkManager · last 15 minutes'],
    ],
    tools: [
      ['sys.net.snapshot', '42 sockets · 2 active routes'],
      ['app.activity.list', 'Photo Sync is the top uploader'],
      ['log.query', 'No link or DNS failures found'],
    ],
    resultTitle: 'Your network is healthy.',
    result: 'Photo Sync is uploading 1,284 files at 47 Mbps and saturating the uplink. Pausing that job will restore interactive traffic immediately.',
    audit: 'task net-8f21 · 3 reads · no system changes',
  },
  {
    id: 'crash',
    title: 'Explain an app crash',
    subtitle: 'Permissions, history, logs, root cause',
    icon: Bug,
    prompt: 'Why did Photos crash, what could it access, and what happened before it failed?',
    intro: 'I’ll inspect the app manifest, its scoped history, and crash evidence without opening unrelated app data.',
    plan: [
      'Read the Photos capability manifest and current grants.',
      'Inspect its recent app activity and resource history.',
      'Correlate the crash dump with journal and audit events.',
    ],
    scopes: [
      ['app.inspect', 'Photos manifest and grants'],
      ['app.history.read', 'Photos · last 30 minutes'],
      ['log.read', 'Photos crash and journal evidence'],
    ],
    tools: [
      ['app.schema', 'pictures.read · gpu.use · no network'],
      ['app.history', 'Opened panorama-final.tiff'],
      ['log.crash', 'OOM kill at 4.1 GiB resident memory'],
    ],
    resultTitle: 'Photos exceeded its memory boundary.',
    result: 'The 1.8 GiB TIFF expanded to 4.1 GiB during decoding. The OS stopped Photos at its 4 GiB limit. It never had network access.',
    audit: 'task crash-3a17 · app-scoped evidence only',
  },
  {
    id: 'workflow',
    title: 'Cross-app workflow',
    subtitle: 'Files → AI → Mail → Calendar',
    icon: Network,
    prompt: 'Summarize the Q3 plan, draft an email to the team, and schedule a review Friday.',
    intro: 'I’ll compose typed operations from four apps and stop before sending or creating anything until you approve.',
    plan: [
      'Read Q3-plan.md from Files and summarize it with the system AI gate.',
      'Create a Mail draft addressed to the project team.',
      'Create a Calendar review event for Friday at 2 PM.',
    ],
    scopes: [
      ['app.call', 'Files · document.read · Q3-plan.md'],
      ['ai.chat', 'summarize approved project content'],
      ['app.call', 'Mail draft and Calendar event'],
    ],
    tools: [
      ['files.document.read', 'Q3-plan.md · 18 pages'],
      ['ai.chat', 'Configured model selected by policy'],
      ['mail.draft.create', 'Draft saved · not sent'],
      ['calendar.event.create', 'Friday 2:00–2:45 PM'],
    ],
    resultTitle: 'The cross-app workflow is complete.',
    result: 'I summarized six Q3 priorities, created a team email draft, and scheduled a 45-minute review. Nothing was sent without approval.',
    audit: 'task flow-92c4 · 4 app calls · 1 model call',
  },
  {
    id: 'models',
    title: 'Shared AI model',
    subtitle: 'AI for apps without bundled providers',
    icon: BrainCircuit,
    prompt: 'Use the system model to summarize this note for the Notes app.',
    intro: 'Notes can use the OS model layer without bundling a model, provider SDK, credential store, or safety pipeline.',
    plan: [
      'Validate the Notes AI declaration and user consent.',
      'Route the request to the available local model.',
      'Record usage and return only generated text to Notes.',
    ],
    scopes: [
      ['ai.chat', 'Notes · summarize user-authored text'],
      ['model.use', 'qwen3-8b · local NPU'],
      ['audit.write', 'usage, latency, and model identity'],
    ],
    tools: [
      ['ai.policy.check', 'Consent and budget valid'],
      ['model.route', 'qwen3-8b selected · local'],
      ['ai.chat', '214 tokens · 640 ms'],
    ],
    resultTitle: 'Notes used AI without owning an AI stack.',
    result: 'The local model returned the summary and usage metadata. Notes never handled model files, provider credentials, or fallback logic.',
    audit: 'task ai-5d60 · local model · 214 tokens',
  },
  {
    id: 'memory',
    title: 'Memory and history',
    subtitle: 'Recall decisions across app sessions',
    icon: History,
    prompt: 'What did I decide about the launch plan last week?',
    intro: 'I’ll search approved work memory across conversations and app sessions, then cite every conclusion.',
    plan: [
      'Search semantic memory for launch-plan decisions.',
      'Open matching Files, Mail, and Calendar session records.',
      'Synthesize the decision with citations and memory controls.',
    ],
    scopes: [
      ['memory.recall', 'work memories · launch plan'],
      ['session.read', 'Files, Mail, and Calendar matches'],
      ['ai.chat', 'synthesize cited records'],
    ],
    tools: [
      ['memory.search', '7 matches across 3 apps'],
      ['session.read', '3 high-confidence records opened'],
      ['ai.chat', 'Decision synthesized with citations'],
    ],
    resultTitle: 'You chose a three-stage launch.',
    result: 'Internal dogfood starts September 4, private beta September 11, and public beta September 18 after the September 15 go/no-go review.',
    audit: 'task mem-b884 · 3 memories cited · forget controls available',
  },
  {
    id: 'access',
    title: 'App access',
    subtitle: 'Permissions, activity, and AI usage',
    icon: ShieldCheck,
    prompt: 'Show what my apps can access and which apps used AI today.',
    intro: 'I’ll read app manifests, active grants, and today’s audit history, then highlight anything unusual.',
    plan: [
      'List installed app capability declarations and current grants.',
      'Query today’s AI and privileged-operation audit records.',
      'Explain outliers and provide revocation paths.',
    ],
    scopes: [
      ['app.inspect', 'installed manifests and grants'],
      ['audit.read', 'today · app and AI activity'],
      ['caps.list', 'active approvals and expiry'],
    ],
    tools: [
      ['app.list', '70 installed app manifests'],
      ['caps.grants', '31 grants · 2 temporary'],
      ['audit.query', '3 AI calls · 1 denied network call'],
    ],
    resultTitle: 'Your app access matches policy.',
    result: 'Mail and Notes used the system AI gate three times today. Photos has no network access, and Weather’s expired temporary grant was correctly denied.',
    audit: 'task access-0e19 · read-only review · no grants changed',
  },
];

export const scenarioById = new Map(scenarios.map((scenario) => [scenario.id, scenario]));

export function scenarioForPrompt(prompt: string) {
  const normalized = prompt.toLowerCase();
  if (normalized.includes('permission') || normalized.includes('access') || normalized.includes('权限')) {
    return scenarioById.get('access');
  }
  if (normalized.includes('crash') || normalized.includes('崩溃') || normalized.includes('photos')) {
    return scenarioById.get('crash');
  }
  if (
    normalized.includes('workflow')
    || normalized.includes('calendar')
    || normalized.includes('email')
    || normalized.includes('工作流')
  ) {
    return scenarioById.get('workflow');
  }
  if (
    normalized.includes('memory')
    || normalized.includes('history')
    || normalized.includes('launch')
    || normalized.includes('记忆')
  ) {
    return scenarioById.get('memory');
  }
  if (normalized.includes('model') || normalized.includes('ai') || normalized.includes('模型')) {
    return scenarioById.get('models');
  }
  if (
    normalized.includes('network')
    || normalized.includes('slow')
    || normalized.includes('health')
    || normalized.includes('网络')
  ) {
    return scenarioById.get('health');
  }
  return undefined;
}

export function message(id: number, role: MessageItem['role'], text: string): MessageItem {
  return { id, kind: 'message', role, text };
}
