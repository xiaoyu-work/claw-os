import { Send, X, type LucideIcon } from 'lucide-react';
import {
  useEffect,
  useRef,
  useState,
  type FormEvent,
  type ReactNode,
} from 'react';
import ClawOsAiIcon from '@/components/ClawOsAiIcon';

export interface ScriptedAssistantAction {
  id: string;
  label: string;
  detail: string;
  icon: LucideIcon;
  prompt: string;
}

export interface ScriptedAssistantResponse<Result> {
  text: string;
  results?: Result[];
}

interface ScriptedAssistantMessage<Result> {
  id: number;
  role: 'assistant' | 'user';
  text: string;
  results?: Result[];
}

interface ScriptedAssistantPanelProps<Result> {
  panelId: string;
  open: boolean;
  title: string;
  subtitle: string;
  initialMessage: string;
  actions: ScriptedAssistantAction[];
  placeholder: string;
  answer: (prompt: string) => ScriptedAssistantResponse<Result>;
  onClose: () => void;
  renderResult?: (result: Result) => ReactNode;
}

export default function ScriptedAssistantPanel<Result>({
  panelId,
  open,
  title,
  subtitle,
  initialMessage,
  actions,
  placeholder,
  answer,
  onClose,
  renderResult,
}: ScriptedAssistantPanelProps<Result>) {
  const [messages, setMessages] = useState<ScriptedAssistantMessage<Result>[]>([
    { id: 1, role: 'assistant', text: initialMessage },
  ]);
  const [prompt, setPrompt] = useState('');
  const [thinking, setThinking] = useState(false);
  const nextId = useRef(2);
  const pendingTimer = useRef<number | null>(null);
  const conversationRef = useRef<HTMLDivElement>(null);

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
  }, [messages, thinking]);

  const ask = (question: string) => {
    const trimmed = question.trim();
    if (!trimmed || thinking) return;
    setMessages((current) => [
      ...current,
      { id: nextId.current++, role: 'user', text: trimmed },
    ]);
    setPrompt('');
    setThinking(true);
    pendingTimer.current = window.setTimeout(() => {
      const response = answer(trimmed);
      setMessages((current) => [
        ...current,
        { id: nextId.current++, role: 'assistant', ...response },
      ]);
      setThinking(false);
      pendingTimer.current = null;
    }, 400);
  };

  const handleSubmit = (event: FormEvent) => {
    event.preventDefault();
    ask(prompt);
  };

  return (
    <aside
      data-scripted-assistant={panelId}
      aria-label={title}
      className={`${open ? 'flex' : 'hidden'} absolute inset-0 z-50 min-h-0 flex-col border-l sm:static sm:w-80 sm:shrink-0`}
      style={{
        background: 'var(--bg-panel)',
        borderColor: 'rgba(0,0,0,0.10)',
        boxShadow: '-12px 0 32px rgba(0,0,0,0.10)',
      }}
    >
      <header className="flex shrink-0 items-center gap-3 border-b px-3 py-3" style={{ borderColor: 'rgba(0,0,0,0.08)' }}>
        <ClawOsAiIcon size={36} className="rounded-xl shadow-lg shadow-[#005CFE]/20" />
        <div className="min-w-0 flex-1">
          <h2 className="font-semibold text-[var(--text-primary)]">{title}</h2>
          <p className="truncate text-[10px] text-[var(--text-muted)]">{subtitle}</p>
        </div>
        <button
          type="button"
          onClick={onClose}
          className="rounded-lg p-2 text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
          aria-label={`Close ${title}`}
        >
          <X size={16} />
        </button>
      </header>

      <div className="grid shrink-0 grid-cols-2 gap-2 border-b p-3" style={{ borderColor: 'rgba(0,0,0,0.06)' }}>
        {actions.map((action) => {
          const Icon = action.icon;
          return (
            <button
              key={action.id}
              type="button"
              data-assistant-action={action.id}
              onClick={() => ask(action.prompt)}
              disabled={thinking}
              className="rounded-xl border p-2 text-left transition-colors hover:border-[#005CFE]/40 hover:bg-[var(--bg-hover)] disabled:opacity-50"
              style={{ borderColor: 'rgba(0,0,0,0.08)', background: 'var(--bg-window)' }}
            >
              <div className="flex items-center gap-1.5 text-xs font-medium text-[var(--text-primary)]">
                <Icon size={13} className="text-[#005CFE]" />
                {action.label}
              </div>
              <div className="mt-1 truncate text-[10px] text-[var(--text-muted)]">{action.detail}</div>
            </button>
          );
        })}
      </div>

      <div ref={conversationRef} className="min-h-0 flex-1 space-y-3 overflow-y-auto p-3">
        {messages.map((message) => (
          <div key={message.id} className={message.role === 'user' ? 'flex justify-end' : 'flex justify-start'}>
            <div
              className={`max-w-[94%] rounded-2xl px-3 py-2 text-xs leading-relaxed ${
                message.role === 'user'
                  ? 'rounded-br-sm bg-[#005CFE] text-white'
                  : 'rounded-bl-sm text-[var(--text-secondary)]'
              }`}
              style={message.role === 'assistant' ? { background: 'var(--bg-input)' } : undefined}
            >
              <p>{message.text}</p>
              {message.results && message.results.length > 0 && renderResult && (
                <div className="mt-2 space-y-1.5">
                  {message.results.map((result, index) => (
                    <div key={index}>{renderResult(result)}</div>
                  ))}
                </div>
              )}
            </div>
          </div>
        ))}
        {thinking && (
          <div className="flex justify-start">
            <div className="flex items-center gap-1 rounded-2xl rounded-bl-sm px-3 py-3" style={{ background: 'var(--bg-input)' }}>
              {[0, 1, 2].map((dot) => (
                <span
                  key={dot}
                  className="h-1.5 w-1.5 animate-pulse rounded-full bg-[#005CFE]"
                  style={{ animationDelay: `${dot * 120}ms` }}
                />
              ))}
            </div>
          </div>
        )}
      </div>

      <form onSubmit={handleSubmit} className="shrink-0 border-t p-3" style={{ borderColor: 'rgba(0,0,0,0.08)' }}>
        <div className="flex items-end gap-2">
          <textarea
            value={prompt}
            onChange={(event) => setPrompt(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Enter' && !event.shiftKey) {
                event.preventDefault();
                ask(prompt);
              }
            }}
            placeholder={placeholder}
            rows={2}
            className="min-h-11 flex-1 resize-none rounded-xl px-3 py-2 text-xs outline-none"
            style={{
              background: 'var(--bg-input)',
              color: 'var(--text-primary)',
              border: '1px solid rgba(0,0,0,0.08)',
            }}
          />
          <button
            type="submit"
            disabled={!prompt.trim() || thinking}
            className="flex h-11 w-11 shrink-0 items-center justify-center rounded-xl bg-[#005CFE] text-white transition-opacity disabled:opacity-35"
            aria-label={`Ask ${title}`}
          >
            <Send size={16} />
          </button>
        </div>
        <p className="mt-2 text-[9px] text-[var(--text-muted)]">
          UI demo · built-in prompts and responses · no external AI call
        </p>
      </form>
    </aside>
  );
}
