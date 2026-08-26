import { create } from 'zustand';

interface DemoGuideStore {
  active: boolean;
  step: number;
  next: () => void;
  restartFromDesktop: () => void;
  restartInAgent: () => void;
}

const finalStep = 6;

export const useDemoGuideStore = create<DemoGuideStore>((set) => ({
  active: true,
  step: 0,
  next: () =>
    set((state) => (
      state.step >= finalStep
        ? { active: false, step: finalStep }
        : { step: state.step + 1 }
    )),
  restartFromDesktop: () => set({ active: true, step: 0 }),
  restartInAgent: () => set({ active: true, step: 1 }),
}));
