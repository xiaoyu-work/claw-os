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
    eyebrow: 'Step 1 of 7',
    title: 'Open Claw OS Agent',
    description: 'The system Agent is the starting point for questions, app control, memory, models, and permissions.',
    action: 'Open Agent',
    event: 'dblclick',
  },
  {
    target: 'agent-scenario-health',
    eyebrow: 'Step 2 of 7',
    title: 'Choose a system question',
    description: 'Start with network health. Five more complete Agent demos remain available beside it.',
    action: 'Select system health',
  },
  {
    target: 'agent-primary-action',
    eyebrow: 'Step 3 of 7',
    title: 'Ask the Agent',
    description: 'The Agent turns the question into a visible plan before touching system data.',
    action: 'Run guided demo',
  },
  {
    target: 'agent-primary-action',
    eyebrow: 'Step 4 of 7',
    title: 'Review the plan',
    description: 'Every operation is explained before the Agent requests access.',
    action: 'Review requested access',
  },
  {
    target: 'agent-primary-action',
    eyebrow: 'Step 5 of 7',
    title: 'Approve exact access',
    description: 'Only the listed, one-time capability scopes will be available to this task.',
    action: 'Allow once',
  },
  {
    target: 'agent-primary-action',
    eyebrow: 'Step 6 of 7',
    title: 'Run visible tools',
    description: 'Each system call reports its evidence and remains reconstructable in the audit trail.',
    action: 'View result',
  },
  {
    target: 'agent-primary-action',
    eyebrow: 'Step 7 of 7',
    title: 'Explore all six demos',
    description: 'The guided path is complete. You can now run every scenario or use the Agent as an AI chat window.',
    action: 'Finish guided tour',
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
      className="fixed inset-0 z-[10000]"
      onContextMenu={(event) => event.preventDefault()}
      onKeyDown={(event) => event.preventDefault()}
      role="presentation"
    >
      <div className="absolute inset-0" />

      {targetRect && (
        <button
          type="button"
          onClick={activateTarget}
          aria-label={guide.action}
          className="absolute rounded-xl border-2 border-[#005CFE] bg-transparent outline-none"
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
        className={`absolute rounded-2xl border border-white/10 bg-[#111113] text-white shadow-2xl ${compactTooltip ? 'p-3' : 'p-4'}`}
        style={{ top: tooltipTop, left: tooltipLeft, width: tooltipWidth }}
      >
        <div className="mb-2 font-mono text-[10px] font-semibold uppercase tracking-[0.16em] text-[#4f8cff]">
          {guide.eyebrow}
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
