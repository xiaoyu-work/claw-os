import { useEffect, useRef, useState, type ComponentType, type FormEvent } from 'react';
import {
  Activity,
  BrainCircuit,
  Bug,
  Check,
  ChevronRight,
  History,
  LockKeyhole,
  Network,
  RefreshCw,
  Send,
  ShieldCheck,
  Sparkles,
  Square,
  Wrench,
} from 'lucide-react';
import { useDemoGuideStore } from '@/stores/useDemoGuideStore';

interface Message {
  id: number;
  role: 'agent' | 'user';
  text: string;
  tools?: string[];
}

type ScenarioStage = 'ready' | 'plan' | 'scopes' | 'tools' | 'result';

interface Scenario {
  id: string;
  title: string;
  subtitle: string;
  icon: ComponentType<{ size?: number; className?: string }>;
  prompt: string;
  intro: string;
  plan: string[];
  scopes: Array<[string, string]>;
  tools: Array<[string, string]>;
  resultTitle: string;
  result: string;
  audit: string;
}

const scenarios: Scenario[] = [
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
      ['app.list', '18 installed apps'],
      ['caps.grants', '31 grants · 2 temporary'],
      ['audit.query', '3 AI calls · 1 denied network call'],
    ],
    resultTitle: 'Your app access matches policy.',
    result: 'Mail and Notes used the system AI gate three times today. Photos has no network access, and Weather’s expired temporary grant was correctly denied.',
    audit: 'task access-0e19 · read-only review · no grants changed',
  },
];

function answerFor(prompt: string): Pick<Message, 'text' | 'tools'> {
  const normalized = prompt.toLowerCase();
  let scenarioId = 'health';
  if (normalized.includes('permission') || normalized.includes('access') || normalized.includes('权限')) {
    scenarioId = 'access';
  } else if (normalized.includes('crash') || normalized.includes('崩溃')) {
    scenarioId = 'crash';
  } else if (normalized.includes('workflow') || normalized.includes('工作流')) {
    scenarioId = 'workflow';
  } else if (normalized.includes('memory') || normalized.includes('history') || normalized.includes('记忆')) {
    scenarioId = 'memory';
  } else if (normalized.includes('model') || normalized.includes('ai') || normalized.includes('模型')) {
    scenarioId = 'models';
  }
  const matched = scenarios.find((scenario) => scenario.id === scenarioId) ?? scenarios[0];

  return {
    text: matched.result,
    tools: matched.tools.map(([tool]) => tool),
  };
}

function stageLabel(stage: ScenarioStage, guideActive: boolean) {
  if (stage === 'ready') return 'Run guided demo';
  if (stage === 'plan') return 'Review requested access';
  if (stage === 'scopes') return 'Allow once';
  if (stage === 'tools') return 'View result';
  return guideActive ? 'Finish guided tour' : 'Run this demo again';
}

export default function Agent() {
  const guideActive = useDemoGuideStore((state) => state.active);
  const restartInAgent = useDemoGuideStore((state) => state.restartInAgent);
  const [selectedId, setSelectedId] = useState('health');
  const [stage, setStage] = useState<ScenarioStage>('ready');
  const [messages, setMessages] = useState<Message[]>([
    {
      id: 1,
      role: 'agent',
      text: 'I am the Claw OS system Agent. Choose a guided system task or ask me anything in the chat box.',
    },
  ]);
  const [prompt, setPrompt] = useState('');
  const [running, setRunning] = useState(false);
  const nextId = useRef(2);
  const pendingTimer = useRef<number | null>(null);
  const conversationRef = useRef<HTMLDivElement>(null);
  const scenario = scenarios.find((item) => item.id === selectedId) ?? scenarios[0];

  useEffect(() => () => {
    if (pendingTimer.current !== null) window.clearTimeout(pendingTimer.current);
  }, []);

  useEffect(() => {
    const frame = window.requestAnimationFrame(() => {
      conversationRef.current?.scrollTo({
        top: conversationRef.current.scrollHeight,
        behavior: 'smooth',
      });
    });
    return () => window.cancelAnimationFrame(frame);
  }, [messages, running, selectedId, stage]);

  const selectScenario = (id: string) => {
    setSelectedId(id);
    setStage('ready');
    setMessages([
      {
        id: nextId.current++,
        role: 'agent',
        text: 'I am the Claw OS system Agent. Choose a guided system task or ask me anything in the chat box.',
      },
    ]);
  };

  const runPrimaryAction = () => {
    if (stage === 'ready') {
      setMessages((current) => [
        ...current,
        { id: nextId.current++, role: 'user', text: scenario.prompt },
        { id: nextId.current++, role: 'agent', text: scenario.intro },
      ]);
      setStage('plan');
      return;
    }
    if (stage === 'plan') {
      setStage('scopes');
      return;
    }
    if (stage === 'scopes') {
      setStage('tools');
      return;
    }
    if (stage === 'tools') {
      setStage('result');
      return;
    }
    if (!guideActive) setStage('ready');
  };

  const submitPrompt = (event?: FormEvent) => {
    event?.preventDefault();
    const text = prompt.trim();
    if (!text || running) return;

    setMessages((current) => [
      ...current,
      { id: nextId.current++, role: 'user', text },
    ]);
    setPrompt('');
    setRunning(true);

    pendingTimer.current = window.setTimeout(() => {
      const answer = answerFor(text);
      setMessages((current) => [
        ...current,
        { id: nextId.current++, role: 'agent', ...answer },
      ]);
      setRunning(false);
      pendingTimer.current = null;
    }, 650);
  };

  const stop = () => {
    if (pendingTimer.current !== null) window.clearTimeout(pendingTimer.current);
    pendingTimer.current = null;
    setRunning(false);
  };

  const restartGuide = () => {
    selectScenario('health');
    restartInAgent();
  };

  return (
    <div className="flex h-full min-h-0 flex-col text-sm sm:flex-row" style={{ background: '#0a0a0b', color: '#fff' }}>
      <aside className="flex w-full shrink-0 flex-col border-b border-white/[0.06] bg-[#111113] p-3 sm:w-64 sm:border-b-0 sm:border-r sm:p-4">
        <div className="mb-3 flex items-center gap-2 sm:mb-5">
          <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-xl bg-[#005CFE]">
            <Sparkles size={18} />
          </div>
          <div className="min-w-0">
            <div className="font-medium">Claw OS Agent</div>
            <div className="truncate text-[11px] text-white/40">System-level AI and app control</div>
          </div>
        </div>

        <div className="flex gap-2 overflow-x-auto pb-1 sm:flex-1 sm:flex-col sm:overflow-y-auto sm:pb-0">
          {scenarios.map((item) => {
            const Icon = item.icon;
            const selected = item.id === selectedId;
            return (
              <button
                key={item.id}
                data-guide-target={item.id === 'health' ? 'agent-scenario-health' : undefined}
                onClick={() => selectScenario(item.id)}
                className="flex w-44 shrink-0 items-center gap-2.5 rounded-xl border p-2.5 text-left transition-colors sm:w-full"
                style={{
                  background: selected ? 'rgba(0,92,254,0.16)' : 'rgba(255,255,255,0.025)',
                  borderColor: selected ? 'rgba(79,140,255,0.7)' : 'rgba(255,255,255,0.06)',
                }}
              >
                <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-white/[0.05] text-[#4f8cff]">
                  <Icon size={16} />
                </div>
                <div className="min-w-0">
                  <div className="truncate text-xs font-medium text-white/85">{item.title}</div>
                  <div className="truncate text-[10px] text-white/35">{item.subtitle}</div>
                </div>
              </button>
            );
          })}
        </div>

        <div className="mt-3 hidden rounded-xl border border-white/[0.06] bg-white/[0.025] p-3 sm:block">
          <div className="flex items-center gap-2 text-xs text-white/70">
            <LockKeyhole size={14} className="text-[#4f8cff]" />
            Approval-gated
          </div>
          <p className="mt-2 text-[10px] leading-relaxed text-white/35">
            Plans, capability grants, tool evidence, and audit IDs stay visible.
          </p>
        </div>
      </aside>

      <main className="flex min-h-0 min-w-0 flex-1 flex-col">
        <header className="flex h-14 shrink-0 items-center justify-between border-b border-white/[0.06] px-4">
          <div className="min-w-0">
            <div className="truncate font-medium">{scenario.title}</div>
            <div className="truncate text-[11px] text-white/40">{scenario.subtitle}</div>
          </div>
          <div className="flex items-center gap-2">
            {!guideActive && (
              <button
                onClick={restartGuide}
                className="hidden items-center gap-1.5 rounded-full border border-white/[0.07] bg-white/[0.03] px-3 py-1 text-[10px] text-white/45 hover:text-white/75 sm:flex"
              >
                <RefreshCw size={11} />
                Restart guide
              </button>
            )}
            <div className="flex items-center gap-2 rounded-full border border-white/[0.06] bg-white/[0.03] px-3 py-1 text-[10px] text-white/45">
              <span className="h-1.5 w-1.5 rounded-full bg-[#005CFE]" />
              Ready
            </div>
          </div>
        </header>

        <div ref={conversationRef} className="flex-1 space-y-4 overflow-y-auto p-4">
          {messages.map((message) => (
            <div key={message.id} className={`flex ${message.role === 'user' ? 'justify-end' : 'justify-start'}`}>
              <div
                className={`max-w-[88%] rounded-2xl px-4 py-3 text-xs leading-relaxed sm:text-sm ${
                  message.role === 'user'
                    ? 'rounded-br-md bg-[#005CFE] text-white'
                    : 'rounded-bl-md border border-white/[0.07] bg-[#111113] text-white/70'
                }`}
              >
                {message.text}
                {message.tools && (
                  <div className="mt-3 flex flex-wrap gap-1.5">
                    {message.tools.map((tool) => (
                      <span key={tool} className="inline-flex items-center gap-1 rounded-md border border-white/[0.07] bg-white/[0.03] px-2 py-1 font-mono text-[10px] text-white/45">
                        <Wrench size={10} />
                        {tool}
                      </span>
                    ))}
                  </div>
                )}
              </div>
            </div>
          ))}

          {stage === 'ready' && (
            <section className="rounded-2xl border border-white/[0.07] bg-[#111113] p-4">
              <div className="mb-2 font-mono text-[10px] uppercase tracking-[0.16em] text-[#4f8cff]">Guided system task</div>
              <h2 className="text-base font-semibold">{scenario.prompt}</h2>
              <p className="mt-2 text-xs leading-relaxed text-white/45">{scenario.intro}</p>
            </section>
          )}

          {stage === 'plan' && (
            <section className="rounded-2xl border border-white/[0.07] bg-[#111113] p-4">
              <div className="mb-3 flex items-center gap-2 text-xs font-medium text-white/80">
                <Sparkles size={15} className="text-[#4f8cff]" />
                Proposed plan
              </div>
              <div className="space-y-2">
                {scenario.plan.map((item, index) => (
                  <div key={item} className="flex gap-3 rounded-xl bg-white/[0.025] p-3 text-xs text-white/55">
                    <span className="flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-[#005CFE]/20 text-[10px] text-[#7aa7ff]">{index + 1}</span>
                    <span className="leading-relaxed">{item}</span>
                  </div>
                ))}
              </div>
            </section>
          )}

          {stage === 'scopes' && (
            <section className="rounded-2xl border border-amber-400/25 bg-amber-400/[0.04] p-4">
              <div className="mb-1 flex items-center gap-2 text-xs font-medium text-amber-200/85">
                <ShieldCheck size={15} />
                One-time approval required
              </div>
              <p className="mb-3 text-[11px] text-white/35">Only these exact capability scopes will be granted.</p>
              <div className="space-y-2">
                {scenario.scopes.map(([scope, description]) => (
                  <div key={scope} className="flex flex-col justify-between gap-1 rounded-xl border border-white/[0.06] bg-black/15 p-3 sm:flex-row sm:items-center">
                    <code className="text-[11px] font-semibold text-amber-200/80">{scope}</code>
                    <span className="text-[10px] text-white/40">{description}</span>
                  </div>
                ))}
              </div>
            </section>
          )}

          {stage === 'tools' && (
            <section className="rounded-2xl border border-white/[0.07] bg-[#111113] p-4">
              <div className="mb-3 flex items-center gap-2 text-xs font-medium text-white/80">
                <Wrench size={15} className="text-[#4f8cff]" />
                Tool evidence
              </div>
              <div className="space-y-2">
                {scenario.tools.map(([tool, result]) => (
                  <div key={tool} className="flex flex-col justify-between gap-1 rounded-xl bg-white/[0.025] p-3 sm:flex-row sm:items-center">
                    <code className="text-[11px] text-[#7aa7ff]">{tool}</code>
                    <span className="flex items-center gap-1.5 text-[10px] text-white/45">
                      <Check size={11} className="text-emerald-400" />
                      {result}
                    </span>
                  </div>
                ))}
              </div>
            </section>
          )}

          {stage === 'result' && (
            <section className="rounded-2xl border border-[#005CFE]/30 bg-[#005CFE]/[0.08] p-4">
              <div className="mb-2 flex items-center gap-2 font-mono text-[10px] uppercase tracking-[0.16em] text-[#7aa7ff]">
                <Check size={13} />
                Completed
              </div>
              <h2 className="text-base font-semibold">{scenario.resultTitle}</h2>
              <p className="mt-2 text-xs leading-relaxed text-white/60">{scenario.result}</p>
              <div className="mt-4 rounded-lg border border-white/[0.06] bg-black/15 px-3 py-2 font-mono text-[10px] text-white/35">
                {scenario.audit}
              </div>
            </section>
          )}

          {running && (
            <div className="flex justify-start">
              <div className="rounded-2xl rounded-bl-md border border-white/[0.07] bg-[#111113] px-4 py-3 text-white/45">
                Inspecting the demo system…
              </div>
            </div>
          )}
        </div>

        <div className="shrink-0 border-t border-white/[0.06] p-3">
          <button
            type="button"
            data-guide-target="agent-primary-action"
            onClick={runPrimaryAction}
            className="mb-3 flex w-full items-center justify-center gap-2 rounded-xl bg-[#005CFE] px-4 py-2.5 text-xs font-medium text-white transition-opacity hover:opacity-90"
          >
            {stageLabel(stage, guideActive)}
            <ChevronRight size={14} />
          </button>

          <form onSubmit={submitPrompt} className="flex items-end gap-2">
            <textarea
              value={prompt}
              onChange={(event) => setPrompt(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === 'Enter' && !event.shiftKey) {
                  event.preventDefault();
                  submitPrompt();
                }
              }}
              rows={1}
              placeholder="Or ask Claw OS anything…"
              className="min-h-10 flex-1 resize-none rounded-xl border border-white/[0.08] bg-white/[0.04] px-3 py-2 text-xs text-white outline-none placeholder:text-white/25 focus:border-[#005CFE]/60"
            />
            <button
              type={running ? 'button' : 'submit'}
              onClick={running ? stop : undefined}
              className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl border border-white/[0.08] bg-white/[0.06] transition-colors hover:bg-white/[0.1] disabled:opacity-35"
              disabled={!running && !prompt.trim()}
              title={running ? 'Stop' : 'Send'}
            >
              {running ? <Square size={15} fill="currentColor" /> : <Send size={16} />}
            </button>
          </form>
        </div>
      </main>
    </div>
  );
}
