import { useState } from "react";
import { Plus, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import type { ActivityDraft } from "@/lib/activities";

const emptyDraft: ActivityDraft = {
  title: "",
  goal: "",
  completion_criteria: "",
  boundaries: "",
  resources: [],
};

export function ActivityForm({
  initial = emptyDraft,
  busy,
  submitLabel,
  onSubmit,
  onCancel,
}: {
  initial?: ActivityDraft;
  busy: boolean;
  submitLabel: string;
  onSubmit: (draft: ActivityDraft) => Promise<void>;
  onCancel: () => void;
}) {
  const [draft, setDraft] = useState<ActivityDraft>(() => ({
    title: initial.title,
    goal: initial.goal,
    completion_criteria: initial.completion_criteria,
    boundaries: initial.boundaries,
    resources: initial.resources.map((resource) => ({ ...resource })),
  }));

  function resourceField(index: number, field: "label" | "reference", value: string) {
    setDraft((current) => ({
      ...current,
      resources: current.resources.map((resource, position) =>
        position === index ? { ...resource, [field]: value } : resource,
      ),
    }));
  }

  return (
    <form
      className="grid gap-4"
      onSubmit={(event) => {
        event.preventDefault();
        if (!busy && draft.title.trim() && draft.goal.trim()) void onSubmit(draft);
      }}
    >
      <fieldset disabled={busy} className="grid min-w-0 gap-4">
        <label className="grid gap-1.5 text-sm">
          Title
          <Input required maxLength={240} value={draft.title}
            onChange={(event) => setDraft({ ...draft, title: event.target.value })} />
        </label>
        <label className="grid gap-1.5 text-sm">
          Goal
          <Textarea required maxLength={16384} value={draft.goal}
            onChange={(event) => setDraft({ ...draft, goal: event.target.value })} />
        </label>
        <label className="grid gap-1.5 text-sm">
          Completion criteria
          <Textarea maxLength={8192} value={draft.completion_criteria}
            placeholder="What will you check before confirming completion?"
            onChange={(event) => setDraft({ ...draft, completion_criteria: event.target.value })} />
        </label>
        <label className="grid gap-1.5 text-sm">
          Boundaries
          <Textarea maxLength={8192} value={draft.boundaries}
            aria-describedby="activity-boundaries-help"
            onChange={(event) => setDraft({ ...draft, boundaries: event.target.value })} />
        </label>
        <p id="activity-boundaries-help" className="-mt-2 text-xs text-muted-foreground">
          Planning guidance only, not enforced permissions. Ordinary capabilities and approvals still apply.
        </p>
        <div className="grid gap-2">
          <h3 className="text-sm font-medium">Resources</h3>
          <p className="text-xs text-muted-foreground">
            References are displayed as text. Adding one does not open, fetch, or execute it.
          </p>
          {draft.resources.map((resource, index) => (
            <div key={index} className="grid min-w-0 gap-2 rounded-md border p-3">
              <div className="flex items-center gap-2">
                <label className="grid min-w-0 flex-1 gap-1 text-xs">
                  Resource {index + 1} label
                  <Input required maxLength={240} value={resource.label}
                    onChange={(event) => resourceField(index, "label", event.target.value)} />
                </label>
                <Button type="button" variant="ghost" size="icon"
                  aria-label={`Remove resource ${index + 1}`}
                  onClick={() => setDraft({
                    ...draft,
                    resources: draft.resources.filter((_, position) => position !== index),
                  })}>
                  <X className="h-4 w-4" />
                </Button>
              </div>
              <label className="grid gap-1 text-xs">
                Resource {index + 1} reference
                <Input required maxLength={4096} value={resource.reference}
                  onChange={(event) => resourceField(index, "reference", event.target.value)} />
              </label>
            </div>
          ))}
          <Button type="button" size="sm" variant="outline" className="w-fit"
            disabled={draft.resources.length >= 32}
            onClick={() => setDraft({
              ...draft, resources: [...draft.resources, { label: "", reference: "" }],
            })}>
            <Plus className="mr-1 h-3.5 w-3.5" /> Add resource
          </Button>
        </div>
        <div className="flex gap-2">
          <Button type="submit" disabled={!draft.title.trim() || !draft.goal.trim()}>
            {busy ? "Saving…" : submitLabel}
          </Button>
          <Button type="button" variant="outline" onClick={onCancel}>Cancel editing</Button>
        </div>
      </fieldset>
    </form>
  );
}
