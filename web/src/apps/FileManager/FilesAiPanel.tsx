import {
  CopyCheck,
  FileSearch,
  FileText,
  FolderInput,
  HardDrive,
  Send,
  ShieldCheck,
  Sparkles,
  X,
  type LucideIcon,
} from 'lucide-react';
import { useEffect, useMemo, useRef, useState, type FormEvent } from 'react';
import { type FSNode, useFileSystemStore } from '@/stores/useFileSystemStore';

interface FilesAiPanelProps {
  open: boolean;
  currentDirectoryId: string;
  selectedNode: FSNode | null;
  onClose: () => void;
  onRevealNode: (node: FSNode) => void;
}

interface AiMessage {
  id: number;
  role: 'assistant' | 'user';
  text: string;
  nodeIds?: string[];
}

interface AiResponse {
  text: string;
  nodeIds?: string[];
}

interface QuickAction {
  id: string;
  label: string;
  detail: string;
  icon: LucideIcon;
  prompt: string;
}

const initialMessage: AiMessage = {
  id: 1,
  role: 'assistant',
  text: 'Try a built-in prompt to find, summarize, compare, or organize demo files. This preview uses scripted responses and never sends file data to an external model.',
};

const textExtensions = new Set([
  'txt',
  'md',
  'json',
  'js',
  'ts',
  'tsx',
  'jsx',
  'html',
  'css',
  'py',
  'sh',
  'log',
]);

function formatSize(size = 0) {
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
  return `${(size / 1024 / 1024).toFixed(1)} MB`;
}

function canSummarize(node: FSNode) {
  if (node.type !== 'file') return false;
  if (node.mimeType?.startsWith('text/')) return true;
  const extension = node.name.split('.').pop()?.toLowerCase() ?? '';
  return textExtensions.has(extension);
}

function summarizeNode(node: FSNode): AiResponse {
  if (!canSummarize(node) || !node.content) {
    return {
      text: `${node.name} has no extracted text in this demo. I can still explain its metadata, or the system Agent can request document extraction with your approval.`,
      nodeIds: [node.id],
    };
  }

  const lines = node.content
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean);
  const checklist = lines.filter((line) => /^\[[ x]\]/i.test(line));
  if (checklist.length > 0) {
    const completed = checklist.filter((line) => /^\[x\]/i.test(line)).length;
    const remaining = checklist.length - completed;
    return {
      text: `${node.name} is a task list with ${completed} completed items and ${remaining} remaining. The open work is: ${checklist
        .filter((line) => !/^\[x\]/i.test(line))
        .map((line) => line.replace(/^\[ \]\s*/, ''))
        .join('; ')}.`,
      nodeIds: [node.id],
    };
  }

  const bullets = lines
    .filter((line) => /^[-*]\s+/.test(line))
    .map((line) => line.replace(/^[-*]\s+/, ''));
  const opening = lines[0]?.replace(/[:.!]+$/, '') || node.name;
  const detail = bullets.length > 0
    ? ` Key points: ${bullets.slice(0, 3).join('; ')}.`
    : ` ${lines.slice(1, 3).join(' ')}`.trimEnd();

  return {
    text: `${node.name} is about ${opening.toLowerCase()}.${detail}`,
    nodeIds: [node.id],
  };
}

function searchFiles(prompt: string, files: FSNode[]): AiResponse {
  const normalized = prompt.toLowerCase();
  const knownTopics: Array<[string[], string]> = [
    [['agent', '智能体'], 'agent'],
    [['ai', 'model', '模型'], 'ai'],
    [['claw'], 'claw'],
    [['launch', '发布'], 'launch'],
    [['todo', 'task', '任务'], 'todo'],
    [['network', '网络'], 'network'],
  ];
  const topic = knownTopics.find(([terms]) => terms.some((term) => normalized.includes(term)))?.[1];
  const quoted = prompt.match(/["“](.+?)["”]/)?.[1]?.toLowerCase();
  const ignored = new Set([
    'about',
    'files',
    'file',
    'find',
    'show',
    'search',
    'where',
    'with',
    'that',
    'this',
    'please',
  ]);
  const fallbackTerms = normalized
    .replace(/[^\p{L}\p{N}.-]+/gu, ' ')
    .split(' ')
    .filter((term) => term.length > 2 && !ignored.has(term));
  const terms = [quoted, topic, ...fallbackTerms].filter((term): term is string => Boolean(term));
  const matches = files
    .map((node) => {
      const searchable = `${node.name}\n${node.content ?? ''}`.toLowerCase();
      const score = terms.reduce((total, term) => {
        if (!searchable.includes(term)) return total;
        return total + (node.name.toLowerCase().includes(term) ? 3 : 1);
      }, 0);
      return { node, score };
    })
    .filter((match) => match.score > 0)
    .sort((a, b) => b.score - a.score || a.node.name.localeCompare(b.node.name))
    .slice(0, 5)
    .map((match) => match.node);

  if (matches.length === 0) {
    return {
      text: 'I found no matching filenames or approved text content. Try a topic such as “Agent”, “tasks”, or a specific filename.',
    };
  }

  return {
    text: `I found ${matches.length} relevant ${matches.length === 1 ? 'file' : 'files'} by searching names and approved text content.`,
    nodeIds: matches.map((node) => node.id),
  };
}

function answerPrompt(
  prompt: string,
  nodes: FSNode[],
  selectedNode: FSNode | null,
  currentDirectoryId: string,
): AiResponse {
  const normalized = prompt.toLowerCase();
  const files = nodes.filter((node) => node.type === 'file');

  if (normalized.includes('large') || normalized.includes('big') || normalized.includes('大文件')) {
    const largest = [...files]
      .sort((a, b) => (b.size ?? 0) - (a.size ?? 0))
      .slice(0, 5);
    return {
      text: `The largest files use ${formatSize(largest.reduce((total, node) => total + (node.size ?? 0), 0))} combined. Review the archive and video first; I have not deleted anything.`,
      nodeIds: largest.map((node) => node.id),
    };
  }

  if (normalized.includes('duplicate') || normalized.includes('重复')) {
    const groups = new Map<string, FSNode[]>();
    files.forEach((node) => {
      const key = `${node.size ?? 0}:${node.content ?? ''}`;
      groups.set(key, [...(groups.get(key) ?? []), node]);
    });
    const duplicates = [...groups.values()].filter((group) => group.length > 1).flat();
    if (duplicates.length === 0) {
      return {
        text: `I compared ${files.length} files by size and demo content fingerprint and found no exact duplicates. No cleanup is needed.`,
      };
    }
    return {
      text: `I found ${duplicates.length} files in exact duplicate groups. Review them before removing anything.`,
      nodeIds: duplicates.map((node) => node.id),
    };
  }

  if (normalized.includes('organize') || normalized.includes('download') || normalized.includes('整理')) {
    const downloads = files.filter((node) => node.parentId === 'fs-user-dl');
    return {
      text: 'Organization preview: move resume.pdf to Documents and keep webos-update.zip in Downloads under an Archives folder. This is a preview only; no files were changed.',
      nodeIds: downloads.map((node) => node.id),
    };
  }

  if (normalized.includes('summar') || normalized.includes('总结') || normalized.includes('摘要')) {
    const namedNode = files.find((node) => normalized.includes(node.name.toLowerCase()));
    const currentTextFile = files.find(
      (node) => node.parentId === currentDirectoryId && canSummarize(node),
    );
    const target = namedNode ?? (selectedNode?.type === 'file' ? selectedNode : null) ?? currentTextFile;
    if (!target) {
      return {
        text: 'Select a text file or include its filename in your question, and I will summarize its approved content.',
      };
    }
    return summarizeNode(target);
  }

  if (
    normalized.includes('explain')
    || normalized.includes('permission')
    || normalized.includes('metadata')
    || normalized.includes('解释')
    || normalized.includes('权限')
  ) {
    if (!selectedNode) {
      return { text: 'Select a file first and I will explain its type, size, ownership, and permissions.' };
    }
    return {
      text: `${selectedNode.name} is a ${selectedNode.type} owned by ${selectedNode.owner}, uses ${formatSize(selectedNode.size)}, and has ${selectedNode.permissions} permissions.`,
      nodeIds: [selectedNode.id],
    };
  }

  if (
    normalized.includes('what can')
    || normalized.includes('help')
    || normalized.includes('能做什么')
  ) {
    return {
      text: 'I can search filenames and approved text, summarize selected documents, explain metadata and permissions, find large or duplicate files, and preview organization plans. Changes remain approval-gated.',
    };
  }

  return searchFiles(prompt, files);
}

export default function FilesAiPanel({
  open,
  currentDirectoryId,
  selectedNode,
  onClose,
  onRevealNode,
}: FilesAiPanelProps) {
  const nodes = useFileSystemStore((state) => state.nodes);
  const getPath = useFileSystemStore((state) => state.getPath);
  const [messages, setMessages] = useState<AiMessage[]>([initialMessage]);
  const [prompt, setPrompt] = useState('');
  const [thinking, setThinking] = useState(false);
  const nextId = useRef(2);
  const pendingTimer = useRef<number | null>(null);
  const conversationRef = useRef<HTMLDivElement>(null);

  const quickActions = useMemo<QuickAction[]>(() => {
    const summaryTarget = selectedNode?.type === 'file' ? selectedNode.name : 'README.txt';
    return [
      {
        id: 'summarize',
        label: 'Summarize',
        detail: summaryTarget,
        icon: FileText,
        prompt: `Summarize ${summaryTarget}`,
      },
      {
        id: 'large',
        label: 'Large files',
        detail: 'Rank storage use',
        icon: HardDrive,
        prompt: 'Find my largest files',
      },
      {
        id: 'duplicates',
        label: 'Duplicates',
        detail: 'Compare fingerprints',
        icon: CopyCheck,
        prompt: 'Find duplicate files',
      },
      {
        id: 'organize',
        label: 'Organize',
        detail: 'Preview Downloads',
        icon: FolderInput,
        prompt: 'Organize my Downloads folder',
      },
    ];
  }, [selectedNode]);

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
      const response = answerPrompt(trimmed, nodes, selectedNode, currentDirectoryId);
      setMessages((current) => [
        ...current,
        { id: nextId.current++, role: 'assistant', ...response },
      ]);
      setThinking(false);
      pendingTimer.current = null;
    }, 450);
  };

  const handleSubmit = (event: FormEvent) => {
    event.preventDefault();
    ask(prompt);
  };

  return (
    <aside
      data-files-ai="panel"
      aria-label="Files AI assistant"
      className={`${open ? 'flex' : 'hidden'} absolute inset-0 z-50 min-h-0 flex-col border-l sm:static sm:w-[340px] sm:shrink-0`}
      style={{
        background: 'var(--bg-panel)',
        borderColor: 'rgba(0,0,0,0.10)',
        boxShadow: '-12px 0 32px rgba(0,0,0,0.10)',
      }}
    >
      <header className="flex shrink-0 items-center gap-3 border-b px-3 py-3" style={{ borderColor: 'rgba(0,0,0,0.08)' }}>
        <div className="flex h-9 w-9 items-center justify-center rounded-xl bg-[#005CFE] text-white shadow-lg shadow-[#005CFE]/20">
          <Sparkles size={18} />
        </div>
        <div className="min-w-0 flex-1">
          <h2 className="font-semibold text-[var(--text-primary)]">Files AI</h2>
          <p className="text-[10px] text-[var(--text-muted)]">UI demo · built-in prompts and responses</p>
        </div>
        <button
          type="button"
          onClick={onClose}
          className="rounded-lg p-2 text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
          aria-label="Close Files AI"
        >
          <X size={16} />
        </button>
      </header>

      <div className="shrink-0 border-b px-3 py-2" style={{ borderColor: 'rgba(0,0,0,0.06)' }}>
        <div className="flex items-center gap-2 rounded-lg px-2.5 py-2 text-[11px]" style={{ background: 'var(--bg-input)' }}>
          <FileSearch size={14} className="shrink-0 text-[#005CFE]" />
          <span className="min-w-0 truncate text-[var(--text-secondary)]">
            Context: {selectedNode ? selectedNode.name : getPath(currentDirectoryId)}
          </span>
        </div>
        <div className="mt-2 grid grid-cols-2 gap-2">
          {quickActions.map((action) => {
            const Icon = action.icon;
            return (
              <button
                key={action.id}
                type="button"
                data-files-ai-action={action.id}
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
      </div>

      <div ref={conversationRef} className="min-h-0 flex-1 space-y-3 overflow-y-auto p-3">
        {messages.map((message) => (
          <div key={message.id} className={message.role === 'user' ? 'flex justify-end' : 'flex justify-start'}>
            <div
              className={`max-w-[92%] rounded-2xl px-3 py-2 text-xs leading-relaxed ${
                message.role === 'user'
                  ? 'rounded-br-sm bg-[#005CFE] text-white'
                  : 'rounded-bl-sm text-[var(--text-secondary)]'
              }`}
              style={message.role === 'assistant' ? { background: 'var(--bg-input)' } : undefined}
            >
              <p>{message.text}</p>
              {message.nodeIds && message.nodeIds.length > 0 && (
                <div className="mt-2 space-y-1.5">
                  {message.nodeIds.map((nodeId) => {
                    const node = nodes.find((item) => item.id === nodeId);
                    if (!node) return null;
                    return (
                      <button
                        key={node.id}
                        type="button"
                        onClick={() => onRevealNode(node)}
                        className="flex w-full items-center gap-2 rounded-lg border px-2 py-1.5 text-left transition-colors hover:border-[#005CFE]/50"
                        style={{ background: 'var(--bg-window)', borderColor: 'rgba(0,0,0,0.08)' }}
                      >
                        <FileText size={13} className="shrink-0 text-[#005CFE]" />
                        <span className="min-w-0 flex-1">
                          <span className="block truncate text-[11px] font-medium text-[var(--text-primary)]">{node.name}</span>
                          <span className="block truncate text-[9px] text-[var(--text-muted)]">
                            {node.parentId ? getPath(node.parentId) : '/'} · {formatSize(node.size)}
                          </span>
                        </span>
                      </button>
                    );
                  })}
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
            placeholder="Ask about your files…"
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
            aria-label="Ask Files AI"
          >
            <Send size={16} />
          </button>
        </div>
        <div className="mt-2 flex items-center gap-1.5 text-[9px] text-[var(--text-muted)]">
          <ShieldCheck size={11} />
          No external AI call · changes require approval
        </div>
      </form>
    </aside>
  );
}
