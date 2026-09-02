import {
  AlertTriangle,
  ArrowUp,
  Check,
  ChevronDown,
  Inbox,
  ListTodo,
  Loader2,
  Menu,
  MessageSquare,
  Moon,
  Plus,
  RefreshCw,
  Settings,
  ShieldCheck,
  Square,
  Sun,
  Wrench,
  type LucideIcon,
} from 'lucide-react';
import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type FormEvent,
  type ReactNode,
  type RefObject,
} from 'react';
import ClawOsAiIcon from '@/components/ClawOsAiIcon';
import { useDemoGuideStore } from '@/stores/useDemoGuideStore';
import { useSettingsStore } from '@/stores/useSettingsStore';
import {
  message,
  scenarioById,
  scenarioForPrompt,
  scenarios,
  type MessageItem,
  type RunItem,
  type Scenario,
  type TimelineItem,
} from './demo';
import { ApprovalsView, InboxView, SettingsView, TasksView } from './DemoPages';

type AgentView = 'chat' | 'tasks' | 'approvals' | 'inbox' | 'settings';

const navigation: Array<{ id: AgentView; label: string; icon: LucideIcon }> = [
  { id: 'chat', label: 'Chat', icon: MessageSquare },
  { id: 'tasks', label: 'Tasks', icon: ListTodo },
  { id: 'approvals', label: 'Approvals', icon: ShieldCheck },
  { id: 'inbox', label: 'Inbox', icon: Inbox },
  { id: 'settings', label: 'Settings', icon: Settings },
];

const savedSessions = [
  { scenarioId: 'memory', label: 'Launch plan decisions', date: 'Today' },
  { scenarioId: 'access', label: 'Review app access', date: 'Today' },
  { scenarioId: 'crash', label: 'Photos crash report', date: 'Yesterday' },
];


export default function Agent() {
  const theme = useSettingsStore((state) => state.theme);
  const setTheme = useSettingsStore((state) => state.setTheme);
  const restartInAgent = useDemoGuideStore((state) => state.restartInAgent);
  const [view, setView] = useState<AgentView>('chat');
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [items, setItems] = useState<TimelineItem[]>([]);
  const [input, setInput] = useState('');
  const nextId = useRef(1);
  const scrollRef = useRef<HTMLDivElement>(null);
  const composerRef = useRef<HTMLTextAreaElement>(null);

  const runningItem = useMemo(
    () => items.find((item): item is RunItem => item.kind === 'run' && item.phase === 'running'),
    [items],
  );
  const pendingItem = useMemo(
    () => items.find((item): item is RunItem => item.kind === 'run' && item.phase === 'approval'),
    [items],
  );
  const runItems = useMemo(
    () => items.filter((item): item is RunItem => item.kind === 'run'),
    [items],
  );

  useEffect(() => {
    const frame = window.requestAnimationFrame(() => {
      scrollRef.current?.scrollTo({
        top: scrollRef.current.scrollHeight,
        behavior: 'smooth',
      });
    });
    return () => window.cancelAnimationFrame(frame);
  }, [items]);

  useEffect(() => {
    if (!runningItem) return;
    const scenario = scenarioById.get(runningItem.scenarioId);
    if (!scenario) return;

    const timer = window.setTimeout(() => {
      setItems((current) => current.map((item) => {
        if (item.kind !== 'run' || item.id !== runningItem.id || item.phase !== 'running') {
          return item;
        }
        const completedTools = item.completedTools + 1;
        if (completedTools >= scenario.tools.length) {
          return { ...item, phase: 'complete', completedTools: scenario.tools.length };
        }
        return { ...item, completedTools };
      }));
    }, 650);
    return () => window.clearTimeout(timer);
  }, [runningItem?.completedTools, runningItem?.id]);

  const rootStyle = {
    '--agent-bg': theme === 'dark' ? '#09090b' : '#ffffff',
    '--agent-fg': theme === 'dark' ? '#fafafa' : '#18181b',
    '--agent-muted': theme === 'dark' ? '#a1a1aa' : '#71717a',
    '--agent-subtle': theme === 'dark' ? '#71717a' : '#a1a1aa',
    '--agent-sidebar': theme === 'dark' ? '#18181b' : '#fafafa',
    '--agent-card': theme === 'dark' ? '#18181b' : '#ffffff',
    '--agent-soft': theme === 'dark' ? '#27272a' : '#f4f4f5',
    '--agent-hover': theme === 'dark' ? '#27272a' : '#f4f4f5',
    '--agent-border': theme === 'dark' ? 'rgba(255,255,255,0.10)' : '#e4e4e7',
    '--agent-primary': theme === 'dark' ? '#fafafa' : '#18181b',
    '--agent-primary-fg': theme === 'dark' ? '#18181b' : '#fafafa',
  } as CSSProperties;

  const updateRun = (id: number, update: Partial<RunItem>) => {
    setItems((current) => current.map((item) => (
      item.kind === 'run' && item.id === id ? { ...item, ...update } : item
    )));
  };

  const newChat = () => {
    setItems([]);
    setInput('');
    setView('chat');
    setSidebarOpen(false);
    window.requestAnimationFrame(() => composerRef.current?.focus());
  };

  const chooseScenario = (scenario: Scenario) => {
    setView('chat');
    setInput(scenario.prompt);
    setSidebarOpen(false);
    window.requestAnimationFrame(() => composerRef.current?.focus());
  };

  const loadCompletedScenario = (scenarioId: string) => {
    const scenario = scenarioById.get(scenarioId);
    if (!scenario) return;
    const userId = nextId.current++;
    const assistantId = nextId.current++;
    const runId = nextId.current++;
    setItems([
      message(userId, 'user', scenario.prompt),
      message(assistantId, 'assistant', scenario.intro),
      {
        id: runId,
        kind: 'run',
        scenarioId,
        phase: 'complete',
        completedTools: scenario.tools.length,
        auditOpen: false,
      },
    ]);
    setView('chat');
    setSidebarOpen(false);
  };

  const submitPrompt = (event?: FormEvent) => {
    event?.preventDefault();
    const text = input.trim();
    if (!text || runningItem) return;

    const scenario = scenarioForPrompt(text);
    const userId = nextId.current++;
    const assistantId = nextId.current++;
    setInput('');

    if (!scenario) {
      setItems((current) => [
        ...current,
        message(userId, 'user', text),
        message(
          assistantId,
          'assistant',
          'This public demo uses recorded Claw OS tasks. Try a system-health, app-crash, cross-app workflow, model, memory, or app-access request.',
        ),
      ]);
      return;
    }

    const runId = nextId.current++;
    setItems((current) => [
      ...current,
      message(userId, 'user', text),
      message(assistantId, 'assistant', scenario.intro),
      {
        id: runId,
        kind: 'run',
        scenarioId: scenario.id,
        phase: 'approval',
        completedTools: 0,
        auditOpen: false,
      },
    ]);
  };

  const stopRunning = () => {
    if (runningItem) updateRun(runningItem.id, { phase: 'stopped' });
  };

  const restartGuide = () => {
    newChat();
    restartInAgent();
  };

  const navigate = (nextView: AgentView) => {
    setView(nextView);
    setSidebarOpen(false);
  };

  return (
    <div
      className="relative flex h-full min-h-0 overflow-hidden bg-[var(--agent-bg)] text-[var(--agent-fg)]"
      style={rootStyle}
    >
      {sidebarOpen && (
        <button
          type="button"
          aria-label="Close navigation"
          onClick={() => setSidebarOpen(false)}
          className="absolute inset-0 z-20 bg-black/30 md:hidden"
        />
      )}

      <aside
        className={`absolute inset-y-0 left-0 z-30 flex w-56 shrink-0 flex-col border-r border-[var(--agent-border)] bg-[var(--agent-sidebar)] transition-transform md:relative md:translate-x-0 ${
          sidebarOpen ? 'translate-x-0' : '-translate-x-full'
        }`}
      >
        <div className="space-y-2 border-b border-[var(--agent-border)] p-3">
          <div className="flex items-center justify-between px-1 py-0.5">
            <div className="flex items-center gap-2">
              <ClawOsAiIcon size={26} />
              <div className="leading-tight">
                <div className="text-sm font-semibold tracking-tight">Claw OS</div>
                <div className="text-[10px] text-[var(--agent-muted)]">claw-os</div>
              </div>
            </div>
            <button
              type="button"
              onClick={() => setTheme(theme === 'dark' ? 'light' : 'dark')}
              className="grid h-7 w-7 place-items-center rounded-md text-[var(--agent-muted)] hover:bg-[var(--agent-hover)] hover:text-[var(--agent-fg)]"
              aria-label={`Switch to ${theme === 'dark' ? 'light' : 'dark'} mode`}
            >
              {theme === 'dark' ? <Sun size={14} /> : <Moon size={14} />}
            </button>
          </div>
          <button
            type="button"
            onClick={newChat}
            className="flex h-8 w-full items-center gap-2 rounded-md bg-[var(--agent-primary)] px-3 text-xs font-medium text-[var(--agent-primary-fg)]"
          >
            <Plus size={14} />
            New chat
          </button>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto p-2">
          <SidebarSection label="Navigation">
            {navigation.map((item) => {
              const Icon = item.icon;
              const active = view === item.id;
              const badge = item.id === 'approvals' && pendingItem ? 1 : 0;
              return (
                <button
                  key={item.id}
                  type="button"
                  onClick={() => navigate(item.id)}
                  className={`flex h-8 w-full items-center gap-2 rounded-md px-2 text-xs transition-colors ${
                    active
                      ? 'bg-[var(--agent-soft)] font-medium text-[var(--agent-fg)]'
                      : 'text-[var(--agent-muted)] hover:bg-[var(--agent-hover)] hover:text-[var(--agent-fg)]'
                  }`}
                >
                  <Icon size={15} />
                  <span>{item.label}</span>
                  {badge > 0 && (
                    <span className="ml-auto grid h-4 min-w-4 place-items-center rounded-full bg-amber-500 px-1 text-[9px] font-semibold text-white">
                      {badge}
                    </span>
                  )}
                </button>
              );
            })}
          </SidebarSection>

          <SidebarSection label="Sessions">
            <div className="px-2 pb-1 pt-2 text-[9px] font-medium uppercase tracking-wider text-[var(--agent-subtle)]">
              Today
            </div>
            {savedSessions.filter((session) => session.date === 'Today').map((session) => (
              <SessionButton
                key={session.scenarioId}
                label={session.label}
                onClick={() => loadCompletedScenario(session.scenarioId)}
              />
            ))}
            <div className="px-2 pb-1 pt-3 text-[9px] font-medium uppercase tracking-wider text-[var(--agent-subtle)]">
              Yesterday
            </div>
            {savedSessions.filter((session) => session.date === 'Yesterday').map((session) => (
              <SessionButton
                key={session.scenarioId}
                label={session.label}
                onClick={() => loadCompletedScenario(session.scenarioId)}
              />
            ))}
          </SidebarSection>
        </div>

        <div className="border-t border-[var(--agent-border)] p-3">
          <button
            type="button"
            onClick={() => navigate('settings')}
            className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left hover:bg-[var(--agent-hover)]"
          >
            <div className="grid h-7 w-7 place-items-center rounded-full bg-[var(--agent-primary)] text-[10px] font-semibold text-[var(--agent-primary-fg)]">
              C
            </div>
            <div className="min-w-0 flex-1 leading-tight">
              <div className="truncate text-xs font-medium">claw-os</div>
              <div className="flex items-center gap-1 text-[9px] text-[var(--agent-muted)]">
                <span className="h-1.5 w-1.5 rounded-full bg-emerald-500" />
                <span className="truncate">llama_local · qwen3-8b</span>
              </div>
            </div>
          </button>
        </div>
      </aside>

      <main className="flex min-h-0 min-w-0 flex-1 flex-col">
        <header className="flex h-12 shrink-0 items-center gap-2 border-b border-[var(--agent-border)] px-3">
          <button
            type="button"
            onClick={() => setSidebarOpen(true)}
            className="grid h-8 w-8 place-items-center rounded-md text-[var(--agent-muted)] hover:bg-[var(--agent-hover)] md:hidden"
            aria-label="Open navigation"
          >
            <Menu size={16} />
          </button>
          <div className="hidden h-5 w-px bg-[var(--agent-border)] md:block" />
          <h1 className="text-sm font-medium">{navigation.find((item) => item.id === view)?.label}</h1>
          <div className="ml-auto flex items-center gap-2">
            <span className="hidden rounded-full border border-[var(--agent-border)] px-2 py-1 text-[9px] text-[var(--agent-muted)] sm:inline">
              Interactive demo
            </span>
            <button
              type="button"
              onClick={restartGuide}
              className="flex h-7 items-center gap-1.5 rounded-md px-2 text-[10px] text-[var(--agent-muted)] hover:bg-[var(--agent-hover)] hover:text-[var(--agent-fg)]"
            >
              <RefreshCw size={11} />
              Guide
            </button>
          </div>
        </header>

        {view === 'chat' && (
          <ChatView
            items={items}
            input={input}
            setInput={setInput}
            runningItem={runningItem}
            scrollRef={scrollRef}
            composerRef={composerRef}
            onChooseScenario={chooseScenario}
            onSubmit={submitPrompt}
            onStop={stopRunning}
            onApprove={(id) => updateRun(id, { phase: 'running', completedTools: 0 })}
            onDeny={(id) => updateRun(id, { phase: 'denied' })}
            onToggleAudit={(id, auditOpen) => updateRun(id, { auditOpen })}
          />
        )}
        {view === 'tasks' && (
          <TasksView
            runs={runItems}
            onOpen={(scenarioId) => loadCompletedScenario(scenarioId)}
            onStop={(id) => updateRun(id, { phase: 'stopped' })}
            onResume={(id) => updateRun(id, { phase: 'running' })}
          />
        )}
        {view === 'approvals' && (
          <ApprovalsView
            pending={pendingItem}
            onApprove={(id) => updateRun(id, { phase: 'running', completedTools: 0 })}
            onDeny={(id) => updateRun(id, { phase: 'denied' })}
          />
        )}
        {view === 'inbox' && <InboxView onOpen={(scenarioId) => loadCompletedScenario(scenarioId)} />}
        {view === 'settings' && <SettingsView />}
      </main>
    </div>
  );
}

function SidebarSection({ label, children }: { label: string; children: ReactNode }) {
  return (
    <section className="mb-4">
      <div className="flex h-7 items-center gap-1 px-2 text-[10px] font-medium text-[var(--agent-muted)]">
        <ChevronDown size={12} />
        {label}
      </div>
      {children}
    </section>
  );
}

function SessionButton({ label, onClick }: { label: string; onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="block h-8 w-full truncate rounded-md px-2 text-left text-[11px] text-[var(--agent-muted)] hover:bg-[var(--agent-hover)] hover:text-[var(--agent-fg)]"
    >
      {label}
    </button>
  );
}

interface ChatViewProps {
  items: TimelineItem[];
  input: string;
  setInput: (value: string) => void;
  runningItem?: RunItem;
  scrollRef: RefObject<HTMLDivElement | null>;
  composerRef: RefObject<HTMLTextAreaElement | null>;
  onChooseScenario: (scenario: Scenario) => void;
  onSubmit: (event?: FormEvent) => void;
  onStop: () => void;
  onApprove: (id: number) => void;
  onDeny: (id: number) => void;
  onToggleAudit: (id: number, open: boolean) => void;
}

function ChatView({
  items,
  input,
  setInput,
  runningItem,
  scrollRef,
  composerRef,
  onChooseScenario,
  onSubmit,
  onStop,
  onApprove,
  onDeny,
  onToggleAudit,
}: ChatViewProps) {
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div ref={scrollRef} className="min-h-0 flex-1 overflow-y-auto px-4">
        <div className="mx-auto flex max-w-3xl flex-col gap-5 py-7">
          {items.length === 0 ? (
            <EmptyState onChooseScenario={onChooseScenario} />
          ) : (
            items.map((item) => (
              item.kind === 'message'
                ? <ChatMessage key={item.id} item={item} />
                : (
                  <RunTrace
                    key={item.id}
                    run={item}
                    onApprove={() => onApprove(item.id)}
                    onDeny={() => onDeny(item.id)}
                    onToggleAudit={(open) => onToggleAudit(item.id, open)}
                  />
                )
            ))
          )}
        </div>
      </div>

      <div className="shrink-0 border-t border-[var(--agent-border)] bg-[var(--agent-bg)]/95 px-4 py-3 backdrop-blur">
        <form onSubmit={onSubmit} className="mx-auto flex max-w-3xl items-end gap-2">
          <textarea
            ref={composerRef}
            value={input}
            onChange={(event) => setInput(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Enter' && !event.shiftKey) {
                event.preventDefault();
                onSubmit();
              }
            }}
            rows={1}
            placeholder="Ask qwen3-8b…"
            className="min-h-11 max-h-32 flex-1 resize-none rounded-md border border-[var(--agent-border)] bg-[var(--agent-bg)] px-3 py-2.5 text-sm text-[var(--agent-fg)] outline-none placeholder:text-[var(--agent-subtle)] focus:ring-2 focus:ring-[var(--agent-muted)]/30"
          />
          {runningItem ? (
            <button
              type="button"
              onClick={onStop}
              className="grid h-11 w-11 place-items-center rounded-md bg-red-600 text-white hover:bg-red-500"
              title="Stop"
            >
              <Square size={15} fill="currentColor" />
            </button>
          ) : (
            <button
              type="submit"
              data-guide-target="agent-primary-action"
              disabled={!input.trim()}
              className="grid h-11 w-11 place-items-center rounded-md bg-[var(--agent-primary)] text-[var(--agent-primary-fg)] disabled:opacity-35"
              title="Send"
            >
              <ArrowUp size={17} />
            </button>
          )}
        </form>
        <p className="mx-auto mt-2 max-w-3xl text-center text-[9px] text-[var(--agent-subtle)]">
          Demo responses are recorded locally. Claw OS still applies capability checks before real actions.
        </p>
      </div>
    </div>
  );
}

function EmptyState({ onChooseScenario }: { onChooseScenario: (scenario: Scenario) => void }) {
  return (
    <div className="mx-auto flex min-h-[410px] w-full max-w-2xl flex-col items-center justify-center py-6 text-center">
      <ClawOsAiIcon size={42} className="mb-4" />
      <h2 className="text-2xl font-semibold tracking-tight">What can I help with?</h2>
      <p className="mt-2 max-w-md text-sm leading-relaxed text-[var(--agent-muted)]">
        Ask the system Agent to inspect Linux, work across apps, use shared models, or recall approved history.
      </p>
      <div className="mt-7 grid w-full grid-cols-1 gap-2 sm:grid-cols-2">
        {scenarios.map((scenario) => {
          const Icon = scenario.icon;
          return (
            <button
              key={scenario.id}
              type="button"
              data-guide-target={scenario.id === 'health' ? 'agent-scenario-health' : undefined}
              onClick={() => onChooseScenario(scenario)}
              className="group flex min-h-16 items-center gap-3 rounded-lg border border-[var(--agent-border)] bg-[var(--agent-card)] p-3 text-left transition-colors hover:bg-[var(--agent-hover)]"
            >
              <span className="grid h-8 w-8 shrink-0 place-items-center rounded-md bg-[var(--agent-soft)] text-[var(--agent-muted)] group-hover:text-[var(--agent-fg)]">
                <Icon size={15} />
              </span>
              <span className="min-w-0">
                <span className="block truncate text-xs font-medium">{scenario.title}</span>
                <span className="mt-0.5 block truncate text-[10px] text-[var(--agent-muted)]">
                  {scenario.subtitle}
                </span>
              </span>
            </button>
          );
        })}
      </div>
    </div>
  );
}

function ChatMessage({ item }: { item: MessageItem }) {
  if (item.role === 'user') {
    return (
      <div className="flex justify-end">
        <div className="max-w-[82%] rounded-2xl rounded-br-md bg-[var(--agent-primary)] px-4 py-2.5 text-sm leading-relaxed text-[var(--agent-primary-fg)]">
          {item.text}
        </div>
      </div>
    );
  }
  return (
    <div className="flex gap-3">
      <ClawOsAiIcon size={20} className="mt-0.5" />
      <p className="min-w-0 flex-1 whitespace-pre-wrap text-sm leading-relaxed">{item.text}</p>
    </div>
  );
}

function RunTrace({
  run,
  onApprove,
  onDeny,
  onToggleAudit,
}: {
  run: RunItem;
  onApprove: () => void;
  onDeny: () => void;
  onToggleAudit: (open: boolean) => void;
}) {
  const scenario = scenarioById.get(run.scenarioId);
  if (!scenario) return null;

  return (
    <div className="ml-0 space-y-3 sm:ml-8">
      <section className="rounded-lg border border-[var(--agent-border)] bg-[var(--agent-card)]">
        <div className="flex items-center justify-between border-b border-[var(--agent-border)] px-3 py-2">
          <span className="text-xs font-medium">Plan</span>
          <span className="text-[9px] text-[var(--agent-muted)]">
            {run.phase === 'complete' ? `${scenario.plan.length}/${scenario.plan.length} complete` : `${Math.min(run.completedTools, scenario.plan.length)}/${scenario.plan.length}`}
          </span>
        </div>
        <div className="space-y-1 p-2">
          {scenario.plan.map((step, index) => {
            const complete = run.phase === 'complete' || index < run.completedTools;
            const active = run.phase === 'running' && index === run.completedTools;
            return (
              <div key={step} className="flex items-start gap-2 rounded-md px-2 py-1.5 text-[11px]">
                <span className={`mt-0.5 grid h-4 w-4 shrink-0 place-items-center rounded-full border text-[8px] ${
                  complete
                    ? 'border-emerald-500 bg-emerald-500 text-white'
                    : active
                      ? 'border-blue-500 text-blue-500'
                      : 'border-[var(--agent-border)] text-[var(--agent-muted)]'
                }`}
                >
                  {complete ? <Check size={9} /> : index + 1}
                </span>
                <span className={complete ? 'text-[var(--agent-muted)]' : ''}>{step}</span>
              </div>
            );
          })}
        </div>
      </section>

      {run.phase === 'approval' && (
        <section className="rounded-lg border border-amber-500/35 bg-amber-500/[0.06] p-3">
          <div className="flex items-center gap-2 text-xs font-medium">
            <ShieldCheck size={15} className="text-amber-500" />
            Approval required
          </div>
          <p className="mt-1 text-[10px] text-[var(--agent-muted)]">
            Allow these exact scopes for this task only.
          </p>
          <div className="mt-3 space-y-1.5">
            {scenario.scopes.map(([scope, description]) => (
              <div
                key={scope}
                className="flex flex-col gap-1 rounded-md border border-[var(--agent-border)] bg-[var(--agent-bg)]/50 px-3 py-2 sm:flex-row sm:items-center sm:justify-between"
              >
                <code className="text-[10px] font-semibold text-amber-600 dark:text-amber-300">{scope}</code>
                <span className="text-[9px] text-[var(--agent-muted)]">{description}</span>
              </div>
            ))}
          </div>
          <div className="mt-3 flex justify-end gap-2">
            <button
              type="button"
              onClick={onDeny}
              className="h-8 rounded-md border border-[var(--agent-border)] px-3 text-[10px] hover:bg-[var(--agent-hover)]"
            >
              Deny
            </button>
            <button
              type="button"
              data-guide-target={scenario.id === 'health' ? 'agent-approval-action' : undefined}
              onClick={onApprove}
              className="h-8 rounded-md bg-[var(--agent-primary)] px-3 text-[10px] font-medium text-[var(--agent-primary-fg)]"
            >
              Allow once
            </button>
          </div>
        </section>
      )}

      {(run.phase === 'running' || run.phase === 'complete' || run.phase === 'stopped') && (
        <div className="space-y-2">
          {scenario.tools.map(([tool, result], index) => {
            const complete = index < run.completedTools;
            const active = run.phase === 'running' && index === run.completedTools;
            if (!complete && !active) return null;
            return (
              <div
                key={tool}
                className="flex items-center justify-between gap-3 rounded-lg border border-[var(--agent-border)] bg-[var(--agent-card)] px-3 py-2 text-[10px]"
              >
                <span className="flex min-w-0 items-center gap-2">
                  <Wrench size={13} className="shrink-0 text-[var(--agent-muted)]" />
                  <code className="truncate font-semibold">{tool}</code>
                  <span className="hidden truncate text-[var(--agent-muted)] sm:inline">
                    {complete ? result : 'running…'}
                  </span>
                </span>
                {active ? (
                  <Loader2 size={12} className="shrink-0 animate-spin text-[var(--agent-muted)]" />
                ) : (
                  <Check size={12} className="shrink-0 text-emerald-500" />
                )}
              </div>
            );
          })}
        </div>
      )}

      {run.phase === 'complete' && (
        <section className="space-y-2 pt-1">
          <div className="flex items-start gap-3">
            <span className="mt-0.5 grid h-5 w-5 shrink-0 place-items-center rounded-full bg-emerald-500 text-white">
              <Check size={12} />
            </span>
            <div>
              <h3 className="text-sm font-semibold">{scenario.resultTitle}</h3>
              <p className="mt-1 text-sm leading-relaxed text-[var(--agent-muted)]">{scenario.result}</p>
            </div>
          </div>
          <div className="flex flex-wrap gap-2 pl-8">
            <button
              type="button"
              data-guide-target={scenario.id === 'health' ? 'agent-result-action' : undefined}
              onClick={() => onToggleAudit(!run.auditOpen)}
              className="h-8 rounded-md border border-[var(--agent-border)] px-3 text-[10px] hover:bg-[var(--agent-hover)]"
            >
              {run.auditOpen ? 'Hide audit' : 'View audit'}
            </button>
          </div>
          {run.auditOpen && (
            <div className="ml-8 rounded-md bg-[var(--agent-soft)] px-3 py-2 font-mono text-[9px] text-[var(--agent-muted)]">
              {scenario.audit}
            </div>
          )}
        </section>
      )}

      {run.phase === 'denied' && (
        <div className="flex items-center gap-2 rounded-lg border border-[var(--agent-border)] px-3 py-2 text-xs text-[var(--agent-muted)]">
          <AlertTriangle size={14} />
          Access was denied. No tools ran and no system state changed.
        </div>
      )}

      {run.phase === 'stopped' && (
        <div className="flex items-center gap-2 rounded-lg border border-[var(--agent-border)] px-3 py-2 text-xs text-[var(--agent-muted)]">
          <Square size={12} />
          Task stopped. Resume it from Tasks when you are ready.
        </div>
      )}
    </div>
  );
}
