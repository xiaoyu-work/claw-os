import { useState } from "react";

import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Textarea } from "@/components/ui/textarea";
import { activityApi, jobStatusLabels, type ActivityDetail } from "@/lib/activities";
import { navigate } from "@/lib/router";

export function ActivityWork({
  detail, disabled, mutate,
}: {
  detail: ActivityDetail;
  disabled: boolean;
  mutate: (action: () => Promise<unknown>, message: string) => Promise<boolean>;
}) {
  const [prompt, setPrompt] = useState("");
  const [sessionId, setSessionId] = useState("");
  const canRun = detail.activity.state === "active";

  return (
    <>
      <Card className="gap-3 p-4">
        <h3 className="text-sm font-medium">Submit or continue work</h3>
        <p className="text-xs text-muted-foreground">
          Work is a durable background job and continues after you leave this page or disconnect.
          Existing capabilities and approval decisions still apply.
        </p>
        {!canRun && <p className="text-sm text-muted-foreground">Resume or reopen this Activity before submitting work.</p>}
        <form className="grid gap-3" onSubmit={(event) => {
          event.preventDefault();
          if (disabled || !canRun) return;
          void mutate(async () => {
            await activityApi.run(detail.activity.id, {
              ...(prompt.trim() ? { prompt } : {}),
              ...(sessionId ? { session_id: sessionId } : {}),
            });
            window.dispatchEvent(new Event("cos:sessions-changed"));
          }, "Work submitted to the durable task queue. Review progress below or in Tasks.")
            .then((ok) => { if (ok) setPrompt(""); });
        }}>
          <fieldset disabled={disabled || !canRun} className="grid min-w-0 gap-3">
            <label className="grid gap-1.5 text-sm">
              Work instructions (optional)
              <Textarea value={prompt} maxLength={524288}
                placeholder="Leave blank to work toward the saved goal."
                onChange={(event) => setPrompt(event.target.value)} />
            </label>
            <label className="grid gap-1.5 text-sm">
              Work session
              <select className="h-9 min-w-0 rounded-md border bg-background px-2 text-sm"
                value={sessionId} onChange={(event) => setSessionId(event.target.value)}>
                <option value="">New session</option>
                {detail.sessions.map((id) => <option key={id} value={id}>Continue {id}</option>)}
              </select>
            </label>
            <Button type="submit" className="w-fit">{sessionId ? "Continue work" : "Submit work"}</Button>
          </fieldset>
        </form>
      </Card>
      <section aria-label="Activity jobs" className="grid min-w-0 gap-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h3 className="text-sm font-medium">Work and results</h3>
          <Button size="sm" variant="outline" onClick={() => navigate("/tasks")}>Open Tasks</Button>
        </div>
        <p className="text-xs text-muted-foreground">
          Job status and result previews come from the task queue. Use Tasks for cancellation and retry.
        </p>
        {detail.jobs.length === 0 && <p className="text-sm text-muted-foreground">No work submitted yet.</p>}
        {detail.jobs.map((job) => (
          <Card key={job.id} className="min-w-0 gap-3 p-4">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <h4 className="break-words text-sm font-medium">{job.title}</h4>
              <span className="rounded-full border px-2 py-0.5 text-xs">{jobStatusLabels[job.status]}</span>
            </div>
            <p className="break-all font-mono text-[11px] text-muted-foreground">{job.id}</p>
            <p className="text-xs text-muted-foreground">
              Created <time dateTime={job.created_at}>{new Date(job.created_at).toLocaleString()}</time>
              {job.finished_at && <> · Finished <time dateTime={job.finished_at}>{new Date(job.finished_at).toLocaleString()}</time></>}
            </p>
            {(job.status === "waiting_approval" || job.waiting_on.length > 0) && (
              <div className="grid gap-2 rounded-md border p-3">
                <p className="text-sm text-amber-600 dark:text-amber-400">Waiting for your approval decision.</p>
                {job.waiting_on.length > 0 && (
                  <ul className="grid gap-1">
                    {job.waiting_on.map((id) => <li key={id} className="break-all font-mono text-xs">{id}</li>)}
                  </ul>
                )}
                <Button size="sm" variant="outline" className="w-fit"
                  onClick={() => navigate("/approvals")}>Open Approvals</Button>
              </div>
            )}
            {job.error && <p className="whitespace-pre-wrap break-words text-sm text-destructive">{job.error}</p>}
            {job.response !== null && (
              <div className="grid gap-1">
                <h5 className="text-xs font-medium">Result preview</h5>
                <pre className="max-h-56 overflow-y-auto whitespace-pre-wrap break-words rounded-md bg-muted p-3 text-xs">
                  {job.response || "No text response."}
                </pre>
              </div>
            )}
            {job.session_id && (
              <Button size="sm" variant="outline" className="w-fit"
                onClick={() => {
                  if (job.session_id) navigate(`/chat/${encodeURIComponent(job.session_id)}`);
                }}>Open session</Button>
            )}
          </Card>
        ))}
        {detail.jobs.length === 100 && (
          <p className="text-xs text-muted-foreground">Showing up to 100 associated jobs. See Tasks for task controls.</p>
        )}
      </section>
      <Card className="gap-3 p-4">
        <h3 className="text-sm font-medium">Sessions</h3>
        {detail.sessions.length === 0 ? <p className="text-sm text-muted-foreground">No associated sessions yet.</p> : (
          <ul className="grid gap-2">
            {detail.sessions.map((id) => (
              <li key={id}>
                <Button size="sm" variant="link" className="h-auto max-w-full whitespace-normal break-all p-0 text-left"
                  onClick={() => navigate(`/chat/${encodeURIComponent(id)}`)}>{id}</Button>
              </li>
            ))}
          </ul>
        )}
      </Card>
    </>
  );
}
