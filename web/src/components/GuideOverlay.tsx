import { useEffect, useState } from 'react';
import { MousePointer2 } from 'lucide-react';
import { useDemoGuideStore } from '@/stores/useDemoGuideStore';

interface GuideStep {
  target: string;
  eyebrow: string;
  title: string;
  description: string;
  action: string;
  event?: 'click' | 'dblclick';
}

const guideSteps: GuideStep[] = [
  {
    target: 'agent-desktop-icon',
    eyebrow: 'Step 1 of 5',
    title: 'Open Claw OS Agent',
    description: 'Open the same Agent workspace that ships with Claw OS.',
    action: 'Open Agent',
    event: 'dblclick',
  },
  {
    target: 'agent-scenario-health',
    eyebrow: 'Step 2 of 5',
    title: 'Start from an example',
    description: 'Examples only prefill the real chat composer. You stay inside the normal Agent interface.',
    action: 'Use system health example',
  },
  {
    target: 'agent-primary-action',
    eyebrow: 'Step 3 of 5',
    title: 'Send the request',
    description: 'The recorded demo follows the same message, plan, and tool layout as the real Agent.',
    action: 'Send request',
  },
  {
    target: 'agent-approval-action',
    eyebrow: 'Step 4 of 5',
    title: 'Approve exact access',
    description: 'The approval stays inline with the conversation and grants only the listed one-time scopes.',
    action: 'Allow once',
  },
  {
    target: 'agent-result-action',
    eyebrow: 'Step 5 of 5',
    title: 'Inspect the evidence',
    description: 'Open the audit reference, then keep chatting or explore Tasks, Approvals, Inbox, and Settings.',
    action: 'Open audit',
  },
];

interface TargetRect {
  top: number;
  left: number;
  width: number;
  height: number;
  bottom: number;
}

export default function GuideOverlay() {
  const active = useDemoGuideStore((state) => state.active);
  const step = useDemoGuideStore((state) => state.step);
  const next = useDemoGuideStore((state) => state.next);
  const dismiss = useDemoGuideStore((state) => state.dismiss);
  const [targetRect, setTargetRect] = useState<TargetRect | null>(null);
  const guide = guideSteps[step];

  useEffect(() => {
    if (!active || !guide) return;

    let frame = 0;
    const update = () => {
      window.cancelAnimationFrame(frame);
      frame = window.requestAnimationFrame(() => {
        const target = document.querySelector<HTMLElement>(`[data-guide-target="${guide.target}"]`);
        if (!target) {
          setTargetRect(null);
          return;
        }
        const rect = target.getBoundingClientRect();
        setTargetRect({
          top: rect.top,
          left: rect.left,
          width: rect.width,
          height: rect.height,
          bottom: rect.bottom,
        });
      });
    };

    update();
    const observer = new MutationObserver(update);
    observer.observe(document.body, { childList: true, subtree: true });
    window.addEventListener('resize', update);
    window.addEventListener('scroll', update, true);

    return () => {
      window.cancelAnimationFrame(frame);
      observer.disconnect();
      window.removeEventListener('resize', update);
      window.removeEventListener('scroll', update, true);
    };
  }, [active, guide]);

  if (!active || !guide) return null;

  const activateTarget = () => {
    const target = document.querySelector<HTMLElement>(`[data-guide-target="${guide.target}"]`);
    if (!target) return;
    if (guide.event === 'dblclick') {
      target.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true, view: window }));
    } else {
      target.click();
    }
    next();
  };

  const tooltipWidth = Math.min(340, window.innerWidth - 24);
  const tooltipLeft = targetRect
    ? Math.max(12, Math.min(window.innerWidth - tooltipWidth - 12, targetRect.left + targetRect.width / 2 - tooltipWidth / 2))
    : (window.innerWidth - tooltipWidth) / 2;
  const compactTooltip = window.innerWidth < 640 && step > 0;
  const tooltipClearance = compactTooltip ? 112 : 190;
  const showBelow = targetRect ? targetRect.bottom + tooltipClearance < window.innerHeight : false;
  const tooltipTop = targetRect
    ? showBelow
      ? targetRect.bottom + 16
      : Math.max(12, targetRect.top - (compactTooltip ? 96 : 176))
    : Math.max(12, window.innerHeight / 2 - 90);
  const backdrop = step === 0
    ? 'rgba(5, 8, 16, 0.68)'
    : 'rgba(5, 8, 16, 0.04)';

  return (
    <div
      className="pointer-events-none fixed inset-0 z-[10000]"
      role="presentation"
    >
      {targetRect && (
        <button
          type="button"
          onClick={activateTarget}
          aria-label={guide.action}
          className="pointer-events-auto absolute rounded-xl border-2 border-[#005CFE] bg-transparent outline-none"
          style={{
            top: targetRect.top - 6,
            left: targetRect.left - 6,
            width: targetRect.width + 12,
            height: targetRect.height + 12,
            boxShadow: `0 0 0 9999px ${backdrop}, 0 0 0 6px rgba(0, 92, 254, 0.22), 0 0 32px rgba(0, 92, 254, 0.9)`,
          }}
        >
          <span className="absolute -right-3 -top-3 flex h-8 w-8 items-center justify-center rounded-full bg-[#005CFE] text-white shadow-lg">
            <MousePointer2 size={15} />
          </span>
        </button>
      )}

      <div
        className={`pointer-events-auto absolute rounded-2xl border border-white/10 bg-[#111113] text-white shadow-2xl ${compactTooltip ? 'p-3' : 'p-4'}`}
        style={{ top: tooltipTop, left: tooltipLeft, width: tooltipWidth }}
      >
        <div className="mb-2 flex items-center justify-between gap-3">
          <span className="font-mono text-[10px] font-semibold uppercase tracking-[0.16em] text-[#4f8cff]">
            {guide.eyebrow}
          </span>
          <button
            type="button"
            onClick={dismiss}
            className="text-[10px] text-white/35 transition-colors hover:text-white/70"
          >
            Skip
          </button>
        </div>
        <h2 className="text-base font-semibold">{guide.title}</h2>
        {!compactTooltip && (
          <p className="mt-2 text-xs leading-relaxed text-white/55">{guide.description}</p>
        )}
        <div className={`${compactTooltip ? 'mt-2' : 'mt-4'} flex items-center gap-2 text-[10px] text-white/35`}>
          <MousePointer2 size={12} />
          <span>Click the highlighted control to continue</span>
        </div>
      </div>
    </div>
  );
}
