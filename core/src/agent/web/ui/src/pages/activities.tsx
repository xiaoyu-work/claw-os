import { useEffect, useRef, useState } from "react";
import { Loader2, Plus, Target } from "lucide-react";

import { ActivityDetailPanel, ActivityStateBadge } from "@/components/activity-detail";
import { ActivityForm } from "@/components/activity-form";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { activityError, useActivities } from "@/hooks/use-activities";
import {
  ACTIVITY_STATES,
  activityApi,
  activityStateLabels,
  isActivityState,
  type ActivityDraft,
  type ActivityState,
} from "@/lib/activities";
import { navigate, useRoute } from "@/lib/router";
import { cn } from "@/lib/utils";

export function ActivitiesPage() {
  const route = useRoute();
  const creating = route === "/activities/new";
  const selectedId = !creating && route.startsWith("/activities/")
    ? route.slice("/activities/".length) : null;
  const [filter, setFilter] = useState<ActivityState>();
  const list = useActivities(filter);

  return (
    <div className="h-full overflow-y-auto p-4 md:p-6">
      <div className="mx-auto grid max-w-7xl gap-5">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div>
            <h1 className="text-xl font-semibold">Activities</h1>
            <p className="text-xs text-muted-foreground">
              Persistent goals, shared with the terminal and native desktop.
            </p>
          </div>
          <Button size="sm" onClick={() => navigate("/activities/new")}>
            <Plus className="mr-1 h-3.5 w-3.5" /> New activity
          </Button>
        </div>
        <div className="grid items-start gap-4 lg:grid-cols-[18rem_minmax(0,1fr)]">
          <section aria-label="Activity list" className="grid min-w-0 gap-3">
            <div className="flex items-center gap-2">
              <select aria-label="Filter activities by state" value={filter ?? ""}
                className="h-9 min-w-0 flex-1 rounded-md border bg-background px-2 text-sm"
                onChange={(event) =>
                  setFilter(isActivityState(event.target.value) ? event.target.value : undefined)}>
                <option value="">All states</option>
                {ACTIVITY_STATES.map((state) => (
                  <option key={state} value={state}>{activityStateLabels[state]}</option>
                ))}
              </select>
              <Button size="sm" variant="outline" disabled={list.loading}
                aria-label="Refresh activity list" onClick={() => void list.refresh()}>
                {list.loading ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : "Refresh"}
              </Button>
            </div>
            {list.error && (
              <p role="alert" className="text-sm text-destructive">
                {list.error}{list.data ? " Showing the last fetched list." : ""}
              </p>
            )}
            {list.loading && !list.data && <p className="text-sm text-muted-foreground">Loading activities…</p>}
            {list.data?.length === 0 && (
              <Card className="gap-2 p-4 text-sm text-muted-foreground">
                {filter ? "No activities in this state." : "No activities yet. Create a goal to get started."}
              </Card>
            )}
            <div className="grid max-h-[65vh] gap-2 overflow-y-auto">
              {list.data?.map((activity) => (
                <button key={activity.id} type="button"
                  aria-label={`Open activity: ${activity.title}`}
                  aria-current={selectedId === activity.id ? "page" : undefined}
                  className={cn(
                    "grid min-w-0 gap-2 rounded-xl border bg-card p-4 text-left shadow-sm hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                    selectedId === activity.id && "border-primary bg-accent",
                  )}
                  onClick={() => navigate(`/activities/${encodeURIComponent(activity.id)}`)}>
                  <span className="break-words text-sm font-medium">{activity.title}</span>
                  <span className="line-clamp-2 break-words text-xs text-muted-foreground">{activity.goal}</span>
                  <ActivityStateBadge state={activity.state} />
                </button>
              ))}
            </div>
            {list.data?.length === 100 && (
              <p className="text-xs text-muted-foreground">Showing up to 100 activities. Filter by state to narrow the list.</p>
            )}
          </section>
          {creating ? (
            <CreateActivity onChanged={list.refresh} />
          ) : selectedId ? (
            <ActivityDetailPanel key={selectedId} id={selectedId} onChanged={list.refresh} />
          ) : (
            <Card className="items-center gap-3 p-8 text-center">
              <Target className="h-8 w-8 text-muted-foreground" />
              <h2 className="text-base font-medium">Choose an Activity</h2>
              <p className="max-w-md text-sm text-muted-foreground">
                Keep a goal, completion criteria, planning boundaries, and references together.
                Submit durable background work, then review the results before confirming completion.
              </p>
            </Card>
          )}
        </div>
      </div>
    </div>
  );
}

function CreateActivity({ onChanged }: { onChanged: () => Promise<unknown> }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const pending = useRef(false);
  const mounted = useRef(true);
  const changed = useRef(onChanged);
  changed.current = onChanged;
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  async function create(draft: ActivityDraft) {
    if (pending.current) return;
    pending.current = true;
    setBusy(true);
    setError(null);
    try {
      const activity = await activityApi.create(draft);
      if (!mounted.current) return;
      await changed.current();
      if (mounted.current) navigate(`/activities/${encodeURIComponent(activity.id)}`);
    } catch (cause) {
      if (mounted.current) setError(activityError(cause));
    } finally {
      pending.current = false;
      if (mounted.current) setBusy(false);
    }
  }

  return (
    <Card className="min-w-0 gap-4 p-5">
      <h2 className="text-lg font-semibold">Create activity</h2>
      <p className="text-xs text-muted-foreground">
        Creating an Activity saves the goal. Work starts only when you submit it.
      </p>
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
      <ActivityForm busy={busy} submitLabel="Create activity" onSubmit={create}
        onCancel={() => navigate("/activities")} />
    </Card>
  );
}
