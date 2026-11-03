import { Globe, Loader2, Monitor, Send, type LucideIcon } from "lucide-react";
import { useState } from "react";

import { type NotificationPreferences, type NotificationSeverity } from "@/lib/notifications";
import { oneOf } from "@/lib/api-shapes";
import { Button } from "./ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "./ui/dialog";
import { Input } from "./ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "./ui/select";
import { Switch } from "./ui/switch";

const SEVERITIES = ["info", "warning", "error", "critical"] as const;
const SEVERITY_LABELS: Record<NotificationSeverity, string> = {
  info: "All updates", warning: "Warnings", error: "Errors", critical: "Critical",
};

export function NotificationSettings({ preferences, onSave, onClose, onSaved }: {
  preferences: NotificationPreferences;
  onSave: (preferences: NotificationPreferences) => Promise<void>;
  onClose: () => void;
  onSaved: () => void;
}) {
  const [draft, setDraft] = useState(preferences);
  const [retention, setRetention] = useState(String(preferences.retention_days));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const save = async () => {
    const days = Number(retention);
    if (!Number.isInteger(days) || days < 1 || days > 365) {
      setError("History retention must be between 1 and 365 days.");
      return;
    }
    if ((draft.dnd_start_minute_utc === undefined) !== (draft.dnd_end_minute_utc === undefined)) {
      setError("Set both quiet-hour times, or clear both.");
      return;
    }
    setSaving(true);
    setError(null);
    try {
      await onSave({ ...draft, retention_days: days });
      setSaving(false);
      onSaved();
    } catch (cause: unknown) {
      setSaving(false);
      setError(cause instanceof Error ? cause.message : "Notification settings could not be saved.");
    }
  };

  return (
    <Dialog open onOpenChange={(open) => { if (!open && !saving) onClose(); }}>
      <DialogContent className="flex max-h-[85vh] flex-col gap-0 overflow-hidden p-0 sm:max-w-xl" showCloseButton={!saving}>
        <DialogHeader className="shrink-0 border-b px-6 py-5 text-left">
          <DialogTitle>Notification settings</DialogTitle>
          <DialogDescription>Choose where and when you receive alerts.</DialogDescription>
        </DialogHeader>
        <form className="flex min-h-0 flex-1 flex-col overflow-hidden" onSubmit={(event) => { event.preventDefault(); void save(); }}>
          <div className="min-h-0 flex-1 overflow-y-auto" data-notification-settings-scroll>
          <fieldset disabled={saving} className="min-w-0 px-6">
            <section className="py-5" aria-label="Delivery channels">
              <h3 className="mb-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground">Delivery</h3>
              <Channel icon={Globe} label="Web alerts" description="Browser delivery and unread badge"
                enabled={draft.web_enabled} severity={draft.web_min_severity}
                onToggle={(web_enabled) => setDraft({ ...draft, web_enabled })}
                onSeverity={(web_min_severity) => setDraft({ ...draft, web_min_severity })} />
              <Channel icon={Monitor} label="Desktop popups" description="Notifications on your Claw desktop"
                enabled={draft.desktop_enabled} severity={draft.desktop_min_severity}
                onToggle={(desktop_enabled) => setDraft({ ...draft, desktop_enabled })}
                onSeverity={(desktop_min_severity) => setDraft({ ...draft, desktop_min_severity })} />
              <Channel icon={Send} label="ntfy push" description="Send alerts to your ntfy topic"
                enabled={draft.ntfy_enabled} severity={draft.ntfy_min_severity}
                onToggle={(ntfy_enabled) => setDraft({ ...draft, ntfy_enabled })}
                onSeverity={(ntfy_min_severity) => setDraft({ ...draft, ntfy_min_severity })} />
              {draft.ntfy_enabled && (
                <label className="mt-3 grid gap-2 pl-11 text-xs">
                  <span className="font-medium">ntfy topic</span>
                  <Input aria-label="ntfy topic" value={draft.ntfy_topic || ""} placeholder="my-claw-alerts"
                    onChange={(event) => setDraft({ ...draft, ntfy_topic: event.target.value || undefined })} />
                </label>
              )}
            </section>

            <section className="border-t py-5" aria-label="Quiet hours">
              <div className="mb-4 flex items-center justify-between">
                <h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">Quiet hours</h3>
                <div className="flex items-center gap-2">
                  {(draft.dnd_start_minute_utc !== undefined || draft.dnd_end_minute_utc !== undefined) && (
                    <Button type="button" size="sm" variant="ghost" className="h-6 px-2 text-[11px]" aria-label="Clear quiet hours"
                      onClick={() => setDraft({ ...draft, dnd_start_minute_utc: undefined, dnd_end_minute_utc: undefined })}>
                      Clear
                    </Button>
                  )}
                  <span className="text-[11px] text-muted-foreground">UTC</span>
                </div>
              </div>
              <div className="flex flex-wrap items-center gap-3">
                <TimeInput label="Quiet hours start" value={draft.dnd_start_minute_utc} onError={setError}
                  onChange={(dnd_start_minute_utc) => setDraft({ ...draft, dnd_start_minute_utc })} />
                <span className="pt-5 text-xs text-muted-foreground">to</span>
                <TimeInput label="Quiet hours end" value={draft.dnd_end_minute_utc} onError={setError}
                  onChange={(dnd_end_minute_utc) => setDraft({ ...draft, dnd_end_minute_utc })} />
              </div>
              <div className="mt-5 flex items-center justify-between gap-4">
                <label htmlFor="critical-alerts" className="text-sm">Allow critical alerts during quiet hours</label>
                <Switch id="critical-alerts" checked={draft.critical_bypasses_dnd}
                  onCheckedChange={(critical_bypasses_dnd) => setDraft({ ...draft, critical_bypasses_dnd })} />
              </div>
            </section>

            <details className="border-t py-4">
              <summary className="cursor-pointer text-sm font-medium">Advanced</summary>
              <div className="mt-4 grid gap-4 pb-1">
                <label className="grid gap-2 text-xs">
                  <span className="font-medium">Muted event kinds</span>
                  <Input aria-label="Muted event kinds" value={draft.muted_kinds.join(", ")} placeholder="cron.completed, agent.completed"
                    onChange={(event) => setDraft({
                      ...draft, muted_kinds: event.target.value.split(",").map((value) => value.trim()).filter(Boolean),
                    })} />
                </label>
                <div className="flex items-center justify-between gap-4">
                  <label htmlFor="notification-retention" className="text-sm">Keep notification history</label>
                  <div className="flex shrink-0 items-center gap-2">
                    <Input id="notification-retention" aria-label="History retention" type="number" min={1} max={365}
                      required className="w-20" value={retention} onChange={(event) => setRetention(event.target.value)} />
                    <span className="text-xs text-muted-foreground">days</span>
                  </div>
                </div>
              </div>
            </details>
          </fieldset>
          </div>
          <div className="shrink-0 border-t px-6 py-4">
            {error && <p role="alert" className="mb-3 text-xs text-destructive">{error}</p>}
            <div className="flex justify-end gap-2">
              <Button type="button" size="sm" variant="ghost" disabled={saving} onClick={onClose}>Cancel</Button>
              <Button type="submit" size="sm" disabled={saving}>
                {saving && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
                Save changes
              </Button>
            </div>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function Channel({ icon: Icon, label, description, enabled, severity, onToggle, onSeverity }: {
  icon: LucideIcon; label: string; description: string; enabled: boolean; severity: NotificationSeverity;
  onToggle: (enabled: boolean) => void; onSeverity: (severity: NotificationSeverity) => void;
}) {
  return (
    <div className="flex flex-wrap items-center justify-between gap-3 py-3">
      <div className="flex min-w-0 items-center gap-3">
        <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-muted">
          <Icon className="h-4 w-4 text-muted-foreground" aria-hidden="true" />
        </span>
        <div><p className="text-sm font-medium">{label}</p><p className="mt-0.5 text-[11px] text-muted-foreground">{description}</p></div>
      </div>
      <div className="ml-auto flex shrink-0 items-center gap-3">
        <Select value={severity} disabled={!enabled} onValueChange={(value) => {
          if (!oneOf(SEVERITIES, value)) throw new Error("Unsupported notification severity.");
          onSeverity(value);
        }}>
          <SelectTrigger size="sm" className="w-32" aria-label={`${label} minimum severity`}><SelectValue /></SelectTrigger>
          <SelectContent>{SEVERITIES.map((value) => <SelectItem key={value} value={value}>{SEVERITY_LABELS[value]}</SelectItem>)}</SelectContent>
        </Select>
        <Switch aria-label={label} checked={enabled} onCheckedChange={onToggle} />
      </div>
    </div>
  );
}

function TimeInput({ label, value, onChange, onError }: {
  label: string; value?: number; onChange: (value?: number) => void; onError: (error: string) => void;
}) {
  const time = value === undefined ? "" : `${String(Math.floor(value / 60)).padStart(2, "0")}:${String(value % 60).padStart(2, "0")}`;
  return (
    <label className="grid gap-1.5 text-xs">
      <span className="text-muted-foreground">{label.endsWith("start") ? "From" : "Until"}</span>
      <Input type="time" aria-label={label} className="w-32" value={time} onChange={(event) => {
        const input = event.target.value;
        if (!input) { onChange(undefined); return; }
        if (!/^(?:[01]\d|2[0-3]):[0-5]\d$/.test(input)) {
          onError("Enter a valid UTC time.");
          return;
        }
        const [hours, minutes] = input.split(":").map(Number);
        onChange(hours * 60 + minutes);
      }} />
    </label>
  );
}
