import { Clock3, ShieldCheck } from 'lucide-react';
import { useState, type ReactNode } from 'react';
import { scenarioById, type RunItem } from './demo';

export function TasksView({
  runs,
  onOpen,
  onStop,
  onResume,
}: {
  runs: RunItem[];
  onOpen: (scenarioId: string) => void;
  onStop: (id: number) => void;
  onResume: (id: number) => void;
}) {
  const visibleRuns = runs.length > 0
    ? runs
    : [
      {
        id: -1,
        kind: 'run' as const,
        scenarioId: 'memory',
        phase: 'complete' as const,
        completedTools: 3,
        auditOpen: false,
      },
    ];

  return (
    <PageFrame
      title="Tasks"
      description="Durable Agent tasks can be inspected, stopped, resumed, and audited."
    >
      <div className="overflow-hidden rounded-lg border border-[var(--agent-border)]">
        <div className="grid grid-cols-[1fr_92px_120px] border-b border-[var(--agent-border)] bg-[var(--agent-soft)] px-4 py-2 text-[9px] font-medium uppercase tracking-wide text-[var(--agent-muted)]">
          <span>Task</span>
          <span>Status</span>
          <span className="text-right">Actions</span>
        </div>
        {visibleRuns.map((run) => {
          const scenario = scenarioById.get(run.scenarioId);
          if (!scenario) return null;
          return (
            <div
              key={run.id}
              className="grid grid-cols-[1fr_92px_120px] items-center border-b border-[var(--agent-border)] px-4 py-3 text-xs last:border-b-0"
            >
              <button type="button" onClick={() => onOpen(run.scenarioId)} className="min-w-0 text-left">
                <span className="block truncate font-medium">{scenario.title}</span>
                <span className="mt-0.5 block truncate text-[9px] text-[var(--agent-muted)]">{scenario.audit}</span>
              </button>
              <span className="flex items-center gap-1.5 text-[10px] capitalize text-[var(--agent-muted)]">
                <span className={`h-1.5 w-1.5 rounded-full ${
                  run.phase === 'complete'
                    ? 'bg-emerald-500'
                    : run.phase === 'running'
                      ? 'bg-blue-500'
                      : run.phase === 'approval'
                        ? 'bg-amber-500'
                        : 'bg-zinc-400'
                }`}
                />
                {run.phase}
              </span>
              <div className="flex justify-end gap-1">
                {run.phase === 'running' && (
                  <SmallButton onClick={() => onStop(run.id)}>Stop</SmallButton>
                )}
                {run.phase === 'stopped' && (
                  <SmallButton onClick={() => onResume(run.id)}>Resume</SmallButton>
                )}
                <SmallButton onClick={() => onOpen(run.scenarioId)}>Open</SmallButton>
              </div>
            </div>
          );
        })}
      </div>
    </PageFrame>
  );
}

export function ApprovalsView({
  pending,
  onApprove,
  onDeny,
}: {
  pending?: RunItem;
  onApprove: (id: number) => void;
  onDeny: (id: number) => void;
}) {
  const scenario = pending ? scenarioById.get(pending.scenarioId) : undefined;
  return (
    <PageFrame
      title="Approvals"
      description="Review pending capability grants and recent decisions."
    >
      <div className="mb-3 flex gap-2 border-b border-[var(--agent-border)]">
        <span className="border-b-2 border-[var(--agent-fg)] px-2 pb-2 text-xs font-medium">
          Pending {pending ? '(1)' : '(0)'}
        </span>
        <span className="px-2 pb-2 text-xs text-[var(--agent-muted)]">Recent</span>
      </div>
      {pending && scenario ? (
        <div className="rounded-lg border border-amber-500/35 bg-amber-500/[0.05] p-4">
          <div className="flex items-start justify-between gap-3">
            <div>
              <h3 className="text-sm font-medium">{scenario.title}</h3>
              <p className="mt-1 text-[10px] text-[var(--agent-muted)]">
                One-time access requested by the current Agent task.
              </p>
            </div>
            <Clock3 size={15} className="text-amber-500" />
          </div>
          <div className="mt-3 space-y-1.5">
            {scenario.scopes.map(([scope, detail]) => (
              <div key={scope} className="flex justify-between gap-3 rounded-md bg-[var(--agent-bg)]/60 px-3 py-2">
                <code className="text-[10px]">{scope}</code>
                <span className="text-right text-[9px] text-[var(--agent-muted)]">{detail}</span>
              </div>
            ))}
          </div>
          <div className="mt-3 flex justify-end gap-2">
            <SmallButton onClick={() => onDeny(pending.id)}>Deny</SmallButton>
            <button
              type="button"
              onClick={() => onApprove(pending.id)}
              className="h-8 rounded-md bg-[var(--agent-primary)] px-3 text-[10px] font-medium text-[var(--agent-primary-fg)]"
            >
              Allow once
            </button>
          </div>
        </div>
      ) : (
        <div className="grid min-h-52 place-items-center rounded-lg border border-dashed border-[var(--agent-border)] text-center">
          <div>
            <ShieldCheck size={24} className="mx-auto text-emerald-500" />
            <p className="mt-2 text-sm font-medium">No pending approvals</p>
            <p className="mt-1 text-[10px] text-[var(--agent-muted)]">
              Capability requests appear here when an Agent task needs access.
            </p>
          </div>
        </div>
      )}
    </PageFrame>
  );
}

export function InboxView({ onOpen }: { onOpen: (scenarioId: string) => void }) {
  const events = [
    {
      scenarioId: 'health',
      title: 'Network activity returned to normal',
      detail: 'Photo Sync completed its upload. No action is required.',
      time: '2 min ago',
      tone: 'bg-emerald-500',
    },
    {
      scenarioId: 'access',
      title: 'Temporary Weather grant expired',
      detail: 'A denied network call was recorded in the audit journal.',
      time: '18 min ago',
      tone: 'bg-amber-500',
    },
    {
      scenarioId: 'memory',
      title: 'Launch review context is ready',
      detail: 'Three approved records were linked to your launch-plan session.',
      time: '1 hr ago',
      tone: 'bg-blue-500',
    },
  ];
  return (
    <PageFrame
      title="Inbox"
      description="Proactive system events and completed Agent work."
    >
      <div className="space-y-2">
        {events.map((event) => (
          <button
            key={event.title}
            type="button"
            onClick={() => onOpen(event.scenarioId)}
            className="flex w-full items-start gap-3 rounded-lg border border-[var(--agent-border)] p-3 text-left hover:bg-[var(--agent-hover)]"
          >
            <span className={`mt-1.5 h-2 w-2 shrink-0 rounded-full ${event.tone}`} />
            <span className="min-w-0 flex-1">
              <span className="block text-xs font-medium">{event.title}</span>
              <span className="mt-1 block text-[10px] leading-relaxed text-[var(--agent-muted)]">{event.detail}</span>
            </span>
            <span className="shrink-0 text-[9px] text-[var(--agent-subtle)]">{event.time}</span>
          </button>
        ))}
      </div>
    </PageFrame>
  );
}

export function SettingsView() {
  const [provider, setProvider] = useState('llama_local');
  const [model, setModel] = useState('qwen3-8b');
  return (
    <PageFrame
      title="Settings"
      description="Configure the same provider and model surfaces used by the real Agent."
    >
      <div className="grid gap-4 md:grid-cols-[150px_1fr]">
        <nav className="space-y-1">
          {['Text model', 'Speech to text', 'Text to speech', 'Image generation', 'Embeddings', 'About'].map((item, index) => (
            <button
              key={item}
              type="button"
              className={`block h-8 w-full rounded-md px-3 text-left text-[10px] ${
                index === 0 ? 'bg-[var(--agent-soft)] font-medium' : 'text-[var(--agent-muted)] hover:bg-[var(--agent-hover)]'
              }`}
            >
              {item}
            </button>
          ))}
        </nav>
        <section className="rounded-lg border border-[var(--agent-border)] p-4">
          <h3 className="text-sm font-medium">Text model</h3>
          <p className="mt-1 text-[10px] text-[var(--agent-muted)]">
            Provider credentials remain in the Claw OS credential store.
          </p>
          <label className="mt-5 block text-[10px] font-medium">
            Provider
            <select
              value={provider}
              onChange={(event) => setProvider(event.target.value)}
              className="mt-1.5 h-9 w-full rounded-md border border-[var(--agent-border)] bg-[var(--agent-bg)] px-3 text-xs outline-none"
            >
              <option value="llama_local">Local llama runtime</option>
              <option value="openai">OpenAI</option>
              <option value="anthropic">Anthropic</option>
              <option value="gemini">Gemini</option>
              <option value="copilot">GitHub Copilot</option>
            </select>
          </label>
          <label className="mt-4 block text-[10px] font-medium">
            Model
            <input
              value={model}
              onChange={(event) => setModel(event.target.value)}
              className="mt-1.5 h-9 w-full rounded-md border border-[var(--agent-border)] bg-[var(--agent-bg)] px-3 text-xs outline-none"
            />
          </label>
          <div className="mt-5 flex items-center justify-between rounded-md bg-[var(--agent-soft)] px-3 py-2">
            <span className="flex items-center gap-2 text-[10px]">
              <span className="h-2 w-2 rounded-full bg-emerald-500" />
              Ready
            </span>
            <span className="font-mono text-[9px] text-[var(--agent-muted)]">{provider} · {model}</span>
          </div>
        </section>
      </div>
    </PageFrame>
  );
}

function PageFrame({
  title,
  description,
  children,
}: {
  title: string;
  description: string;
  children: ReactNode;
}) {
  return (
    <div className="min-h-0 flex-1 overflow-y-auto p-5">
      <div className="mx-auto max-w-4xl">
        <div className="mb-5">
          <h2 className="text-lg font-semibold">{title}</h2>
          <p className="mt-1 text-[11px] text-[var(--agent-muted)]">{description}</p>
        </div>
        {children}
      </div>
    </div>
  );
}

function SmallButton({ children, onClick }: { children: ReactNode; onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="h-7 rounded-md border border-[var(--agent-border)] px-2 text-[9px] hover:bg-[var(--agent-hover)]"
    >
      {children}
    </button>
  );
}
