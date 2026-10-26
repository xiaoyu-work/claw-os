import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { nearbyApi, stageFile, MAX_FILE_BYTES, MAX_TOTAL_BYTES, type NearbyView, type NearbyAction } from "@/lib/nearby";

export function NearbyPage() {
  const [view, setView] = useState<NearbyView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [seconds, setSeconds] = useState(300);
  const [disruption, setDisruption] = useState(false);
  const [peerId, setPeerId] = useState("");
  const [selected, setSelected] = useState<string[]>([]);
  const mounted = useRef(true);
  const pending = useRef(false);
  const pollBlocked = useRef(false);
  const sequence = useRef(0);
  const read = useRef<AbortController | null>(null);

  const refresh = useCallback(async (manual = false) => {
    if (manual) pollBlocked.current = false;
    if (pending.current || pollBlocked.current || !manual && read.current) return;
    read.current?.abort();
    const abort = new AbortController(); read.current = abort;
    const serial = ++sequence.current;
    try {
      const result = await nearbyApi.status(abort.signal);
      if (mounted.current && serial === sequence.current && !abort.signal.aborted) {
        setView(result); setError(null);
      }
    } catch (cause) {
      if (mounted.current && serial === sequence.current && !abort.signal.aborted) {
        pollBlocked.current = true;
        setError(cause instanceof Error ? cause.message : "Could not load nearby sharing.");
      }
    } finally {
      if (read.current === abort) read.current = null;
    }
  }, []);
  useEffect(() => {
    mounted.current = true; void refresh();
    const timer = window.setInterval(() => void refresh(), 1500);
    return () => { mounted.current = false; ++sequence.current; read.current?.abort(); window.clearInterval(timer); };
  }, [refresh]);
  useEffect(() => { setDisruption(false); }, [view?.probe.mode]);
  useEffect(() => {
    if (view?.session && ["ended", "failed", "interrupted"].includes(view.session.phase)) setDisruption(false);
  }, [view?.session?.phase]);
  useEffect(() => { setPeerId(""); }, [view?.session?.id]);
  useEffect(() => { setSelected((ids) => {
    const retained = ids.filter((id) => view?.uploads.some((upload) => upload.id === id));
    return retained.length === ids.length ? ids : retained;
  }); }, [view?.uploads]);

  async function perform(action: NearbyAction) {
    if (!view || pending.current) return;
    pending.current = true; ++sequence.current; read.current?.abort();
    pollBlocked.current = false;
    setBusy(true); setError(null);
    try {
      const result = await nearbyApi.control(action, view.owner_uid);
      if (mounted.current) setView(result.view);
    } catch (cause) {
      pollBlocked.current = true;
      if (mounted.current) setError(`${cause instanceof Error ? cause.message : "Nearby action failed."} Refresh before retrying; the action is not replayed automatically.`);
    } finally {
      pending.current = false; if (mounted.current) setBusy(false);
    }
  }
  async function upload(files: File[]) {
    if (!view || pending.current) return;
    pending.current = true; ++sequence.current; read.current?.abort();
    pollBlocked.current = false;
    setBusy(true); setError(null);
    try {
      if (!files.length || files.length > 16 || files.some((file) => file.size > MAX_FILE_BYTES)
        || files.reduce((sum, file) => sum + file.size, 0) > MAX_TOTAL_BYTES) {
        throw new Error("Select 1-16 files, at most 128 MiB each and 512 MiB total.");
      }
      const ids: string[] = [];
      for (const file of files) {
        ids.push(await stageFile(file, view.owner_uid, (fresh) => { if (mounted.current) setView(fresh); }, () => mounted.current));
      }
      if (mounted.current) setSelected(ids);
    } catch (cause) {
      pollBlocked.current = true;
      if (mounted.current) setError(cause instanceof Error ? cause.message : "Upload failed; inspect retained uploads.");
    } finally {
      pending.current = false; if (mounted.current) setBusy(false);
    }
  }
  const session = view?.session;
  const active = session && ["starting", "ready", "stopping"].includes(session.phase);
  const ready = session?.phase === "ready";
  const disabled = busy || !!error || !view;
  const disruptive = view?.probe.mode !== null && view?.probe.mode !== "shared";
  const canOpen = view?.probe.available && !active && (!disruptive || disruption)
    && Number.isInteger(seconds) && seconds >= 30 && seconds <= 600;
  return (
    <section aria-label="Nearby Sharing" className="h-full overflow-y-auto p-4">
      <div className="mx-auto grid max-w-4xl gap-4">
        <div className="flex items-center justify-between gap-3">
          <h2 className="text-xl font-semibold">Nearby Sharing</h2>
          <Button variant="outline" disabled={busy} onClick={() => void refresh(true)}>Refresh nearby sharing</Button>
        </div>
        <p className="text-sm text-muted-foreground">AirDrop files with an iPhone using its built-in share sheet. No phone App, Agent task or Activity is needed. Set the iPhone to Everyone for 10 minutes.</p>
        <p className="text-xs text-muted-foreground">Device names are unverified. Files and names are untrusted; receiving never opens, executes or uploads them to a model. Interrupted sends are not automatically retried.</p>
        {error && <p role="alert" className="text-destructive">{error} Permission requests can be reviewed in <a className="underline" href="#/approvals">Approvals</a>; retry only the action you intend.</p>}
        {!view ? <p>Loading nearby sharing...</p> : <>
          <Card className="gap-3 p-4">
            <h3 className="font-medium">Radio and receive window</h3>
            <p className="text-sm">Backend: {view.probe.available ? "available" : "unavailable"} · Mode: {view.probe.mode ?? "unsupported"} · Driver: {view.probe.driver ?? "unknown"}</p>
            {view.probe.diagnostics.map((diagnostic, index) => <p key={index} className="text-sm">{diagnostic}</p>)}
            {disruptive && <p className="text-sm font-medium text-amber-600">This radio mode can disconnect Wi-Fi and interrupt remote access or cloud model requests.</p>}
            <label className="flex items-center gap-2 text-sm">
              <input type="checkbox" checked={disruption} disabled={busy || !!active} onChange={(event) => setDisruption(event.target.checked)} />
              I acknowledge possible Wi-Fi interruption
            </label>
            <label className="grid gap-1 text-sm">Window duration in seconds (30-600)
              <Input type="number" min={30} max={600} value={seconds} disabled={busy || !!active} onChange={(event) => setSeconds(Number(event.target.value))} />
            </label>
            <div className="flex flex-wrap gap-2">
              <Button disabled={disabled || !canOpen} onClick={() => {
                if (view.probe.mode) void perform({ action: "open", duration_secs: seconds, receive: false, expected_mode: view.probe.mode, allow_disruption: disruption });
              }}>Open send window</Button>
              <Button disabled={disabled || !canOpen} onClick={() => {
                if (view.probe.mode) void perform({ action: "open", duration_secs: seconds, receive: true, expected_mode: view.probe.mode, allow_disruption: disruption });
              }}>Receive files for this window</Button>
              {active && <Button variant="outline" disabled={busy || session.phase === "stopping"} onClick={() => void perform({ action: "stop", session_id: session.id })}>Stop sharing and restore radio</Button>}
            </div>
            {session && <p role="status" className="text-sm">Window {session.phase} · expires {session.expires_at}{session.receive ? " · receiving with per-offer confirmation" : ""}</p>}
            {session?.message && <p className="text-sm">{session.message}</p>}
          </Card>
          <Card className="gap-3 p-4">
            <h3 className="font-medium">Stage files before activating a disruptive radio</h3>
            <p className="text-xs text-muted-foreground">Up to 16 regular files, 128 MiB each and 512 MiB total. Directories and automatic link opening are not supported.</p>
            <Input aria-label="Files for nearby sharing" type="file" multiple disabled={busy} onChange={(event) => {
              const files = Array.from(event.target.files ?? []); event.target.value = ""; void upload(files);
            }} />
            {view.uploads.map((item) => <div key={item.id} className="flex flex-wrap items-center gap-2 text-sm">
              <label className="flex items-center gap-2">
                <input type="checkbox" checked={selected.includes(item.id)} disabled={busy || !item.ready}
                  onChange={(event) => setSelected((ids) => event.target.checked ? [...ids, item.id] : ids.filter((id) => id !== item.id))} />
                {item.name} · {item.written}/{item.bytes} bytes · {item.ready ? "ready" : "partial"}
              </label>
              <Button size="sm" variant="ghost" disabled={busy} onClick={() => void perform({ action: "upload_delete", upload_id: item.id })}>Delete staged {item.name}</Button>
            </div>)}
          </Card>
          <Card className="gap-3 p-4">
            <h3 className="font-medium">Nearby receivers</h3>
            <Button className="w-fit" variant="outline" disabled={disabled || !ready || !!session?.discovering}
              onClick={() => { if (session) void perform({ action: "discover", session_id: session.id }); }}>
              {session?.discovering ? "Discovering receivers..." : "Discover nearby receivers"}
            </Button>
            {session?.peers.map((peer) => <label key={peer.id} className="flex items-center gap-2 text-sm">
              <input type="radio" name="nearby-peer" value={peer.id} checked={peerId === peer.id} disabled={busy} onChange={() => setPeerId(peer.id)} />
              {peer.name} <code className="text-xs">{peer.id}</code>
            </label>)}
            <Button className="w-fit" disabled={disabled || !ready || !session?.peers.some((peer) => peer.id === peerId)
              || !selected.length || selected.some((id) => !view.uploads.some((item) => item.id === id && item.ready))}
              onClick={() => { if (session) void perform({ action: "send", session_id: session.id, peer_id: peerId, paths: [], uploads: selected }); }}>
              Send selected files to selected device
            </Button>
          </Card>
          <Card className="gap-3 p-4">
            <h3 className="font-medium">Incoming offers</h3>
            <p className="text-xs text-muted-foreground">Confirm each exact offer before its protocol deadline. Timeout declines it; no auto-accept preference exists.</p>
            {session?.offers.map((offer) => <div key={offer.id} className="grid gap-2 border-t pt-2 text-sm">
              <p>{offer.sender} · {offer.phase} · expires {offer.expires_at}</p>
              {offer.files.map((file, index) => <p key={index}>{file.name} · {file.bytes ?? "unknown"} bytes</p>)}
              <div className="flex gap-2">
                <Button disabled={disabled || !ready || offer.phase !== "offered" || Date.parse(offer.expires_at) <= Date.now()}
                  onClick={() => { if (session) void perform({ action: "decide", session_id: session.id, offer_id: offer.id, accept: true }); }}>Accept from {offer.sender}</Button>
                <Button variant="outline" disabled={busy || offer.phase !== "offered"}
                  onClick={() => { if (session) void perform({ action: "decide", session_id: session.id, offer_id: offer.id, accept: false }); }}>Decline from {offer.sender}</Button>
              </div>
            </div>)}
          </Card>
          <Card className="gap-3 p-4">
            <h3 className="font-medium">Transfers</h3>
            <p className="text-xs text-muted-foreground">Inbox: <code>{view.inbox}</code>. Delivered means protocol acknowledgement, not proof a person read the file.</p>
            {session?.transfers.map((transfer) => <div key={transfer.id} className="grid gap-1 border-t pt-2 text-sm">
              <p>{transfer.incoming ? "From" : "To"} {transfer.peer} · {transfer.phase} · {transfer.bytes}/{transfer.total_bytes ?? "unknown"} {transfer.incoming ? "persisted file bytes (live receive progress unavailable)" : "wire bytes"}</p>
              {transfer.files.map((file, index) => <p key={index}>{file.name}</p>)}
              {transfer.received_paths.map((path) => <code key={path} className="break-all">{path}</code>)}
              {transfer.message && <p>{transfer.message}</p>}
              {!transfer.incoming && transfer.phase === "sending" && <Button className="w-fit" variant="outline" disabled={busy || !ready}
                onClick={() => { if (session) void perform({ action: "cancel", session_id: session.id, transfer_id: transfer.id }); }}>Interrupt send to {transfer.peer}</Button>}
            </div>)}
          </Card>
        </>}
      </div>
    </section>
  );
}
