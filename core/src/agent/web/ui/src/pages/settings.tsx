/**
 * Settings page. Sub-nav for the five modalities (llm/embed/tts/stt/imagegen)
 * plus an About panel.
 *
 * Each modality form is built dynamically from the response of
 * `GET /api/setup/providers/{modality}`, which returns the same shape
 * `cos agent setup providers <modality>` prints. Apply hits
 * `POST /api/setup/apply`. Verify hits `POST /api/setup/test/{modality}`.
 * OAuth flows for `llm` use `oauth/start` + `oauth/poll`.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { CheckCircle2, Loader2, RotateCcw, XCircle } from "lucide-react";

import { api } from "@/lib/api";
import { isActive, navigate, useRoute } from "@/lib/router";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

const MODALITIES = [
  { key: "llm", label: "LLM" },
  { key: "embed", label: "Embeddings" },
  { key: "tts", label: "Text → speech" },
  { key: "stt", label: "Speech → text" },
  { key: "imagegen", label: "Image gen" },
];

export function SettingsPage({ meta }: { meta: any }) {
  const route = useRoute();
  const modality = useMemo(() => {
    if (route === "/settings/about") return "about";
    const m = route.match(/^\/settings\/([^/]+)/);
    return m?.[1] || "llm";
  }, [route]);

  return (
    <div className="flex h-full overflow-hidden">
      <aside className="w-56 shrink-0 border-r p-3">
        <h2 className="px-2 pb-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground">
          Settings
        </h2>
        <nav className="grid gap-0.5">
          {MODALITIES.map((m) => (
            <SettingsNavItem
              key={m.key}
              label={m.label}
              href={`/settings/${m.key}`}
              active={isActive(`/settings/${m.key}`, route) || (m.key === "llm" && route === "/settings")}
            />
          ))}
          <div className="my-2 border-t" />
          <SettingsNavItem
            label="About"
            href="/settings/about"
            active={route === "/settings/about"}
          />
        </nav>
      </aside>
      <div className="flex-1 overflow-y-auto">
        {modality === "about" ? (
          <AboutPanel meta={meta} />
        ) : (
          <ModalityPanel key={modality} modality={modality} />
        )}
      </div>
    </div>
  );
}

function SettingsNavItem({
  label,
  href,
  active,
}: {
  label: string;
  href: string;
  active: boolean;
}) {
  return (
    <button
      type="button"
      onClick={() => navigate(href)}
      className={cn(
        "rounded-md px-2 py-1.5 text-left text-sm transition-colors",
        active
          ? "bg-sidebar-accent text-sidebar-accent-foreground"
          : "text-sidebar-foreground/80 hover:bg-sidebar-accent/60",
      )}
    >
      {label}
    </button>
  );
}

type ProviderEntry = {
  id?: string;
  key?: string;
  name?: string;
  label?: string;
  display?: string;
  models?: any[];
  requires_api_key?: boolean;
  supports_oauth?: boolean;
  needs_base_url?: boolean;
  api_key_env?: string;
  default_env?: string;
  default_model?: string;
};

function providerId(p: ProviderEntry): string {
  return String(p.id || p.key || p.label || p.name || "");
}

function ModalityPanel({ modality }: { modality: string }) {
  const [status, setStatus] = useState<any>(null);
  const [providers, setProviders] = useState<ProviderEntry[]>([]);
  const [provider, setProvider] = useState<string>("");
  const [model, setModel] = useState<string>("");
  const [apiKey, setApiKey] = useState<string>("");
  const [baseUrl, setBaseUrl] = useState<string>("");
  const [models, setModels] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<string | null>(null);
  const [msg, setMsg] = useState<{ kind: "ok" | "err"; text: string } | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setMsg(null);
    try {
      const [st, pv] = await Promise.all([
        api.get(`/api/setup/status/${modality}`).catch(() => null),
        api.get(`/api/setup/providers/${modality}`).catch(() => null),
      ]);
      setStatus(st);
      const list = extractProviders(pv);
      setProviders(list);
      if (!provider && list.length > 0) {
        const cur =
          (st as any)?.provider ||
          (st as any)?.current ||
          providerId(list[0]);
        setProvider(String(cur || ""));
      }
      const curModel = (st as any)?.model || (st as any)?.current_model;
      if (curModel) setModel(String(curModel));
      const curBase = (st as any)?.base_url;
      if (curBase) setBaseUrl(String(curBase));
    } catch (e: any) {
      setMsg({ kind: "err", text: e?.message || "Load failed" });
    } finally {
      setLoading(false);
    }
  }, [modality]);

  useEffect(() => {
    load();
  }, [load]);

  // Load model list whenever provider changes. The /api/setup/models
  // endpoint only supports `copilot`; for everything else we fall back
  // to the `models` list returned by /api/setup/providers/{modality}.
  useEffect(() => {
    if (!provider) return;
    const entry = providers.find((p) => providerId(p) === provider);
    if (entry?.models && entry.models.length > 0) {
      setModels(extractModelIds(entry.models));
      return;
    }
    let cancelled = false;
    api
      .get(`/api/setup/models/${modality}/${provider}`)
      .then((r) => {
        if (cancelled) return;
        setModels(extractModelIds(r));
      })
      .catch(() => {
        if (!cancelled) setModels([]);
      });
    return () => {
      cancelled = true;
    };
  }, [modality, provider, providers]);

  const providerEntry = useMemo(
    () => providers.find((p) => providerId(p) === provider),
    [providers, provider],
  );

  async function apply(verify: boolean) {
    setBusy("apply");
    setMsg(null);
    try {
      const body: any = { modality, provider, verify };
      if (model) body.model = model;
      if (apiKey) body.api_key = apiKey;
      if (baseUrl) body.base_url = baseUrl;
      await api.post("/api/setup/apply", body);
      setMsg({ kind: "ok", text: verify ? "Applied and verified." : "Applied." });
      setApiKey("");
      await load();
    } catch (e: any) {
      setMsg({ kind: "err", text: e?.message || "Apply failed" });
    } finally {
      setBusy(null);
    }
  }

  async function reset() {
    if (!confirm(`Reset ${modality} configuration?`)) return;
    setBusy("reset");
    setMsg(null);
    try {
      await api.post(`/api/setup/reset/${modality}`);
      setMsg({ kind: "ok", text: "Reset." });
      setProvider("");
      setModel("");
      setApiKey("");
      setBaseUrl("");
      await load();
    } catch (e: any) {
      setMsg({ kind: "err", text: e?.message || "Reset failed" });
    } finally {
      setBusy(null);
    }
  }

  async function test() {
    setBusy("test");
    setMsg(null);
    try {
      const r = await api.post(`/api/setup/test/${modality}`);
      setMsg({ kind: "ok", text: (r as any)?.message || "Verified." });
    } catch (e: any) {
      setMsg({ kind: "err", text: e?.message || "Verify failed" });
    } finally {
      setBusy(null);
    }
  }

  async function oauthStart() {
    setBusy("oauth");
    setMsg(null);
    try {
      const r: any = await api.post("/api/setup/oauth/start", { provider, modality });
      const code = r?.user_code || r?.verification_code;
      const url = r?.verification_uri || r?.verification_url;
      if (url) window.open(url, "_blank", "noopener");
      const devCode = r?.device_code;
      if (!devCode) throw new Error("No device_code returned");
      setMsg({
        kind: "ok",
        text: `Code: ${code || "(check console)"}. Polling…`,
      });
      const deadline = Date.now() + 10 * 60_000;
      while (Date.now() < deadline) {
        await new Promise((res) => setTimeout(res, (r?.interval || 5) * 1000));
        try {
          const p: any = await api.post("/api/setup/oauth/poll", {
            provider,
            modality,
            device_code: devCode,
          });
          if (p?.status === "ok" || p?.token || p?.access_token) {
            setMsg({ kind: "ok", text: "OAuth complete." });
            await load();
            return;
          }
        } catch {
          // keep polling
        }
      }
      setMsg({ kind: "err", text: "OAuth timed out" });
    } catch (e: any) {
      setMsg({ kind: "err", text: e?.message || "OAuth failed" });
    } finally {
      setBusy(null);
    }
  }

  const ready = (status as any)?.ready === true || (status as any)?.configured === true;
  const supportsOauth = !!providerEntry?.supports_oauth;
  const needsBase = !!providerEntry?.needs_base_url;

  return (
    <div className="mx-auto max-w-2xl p-6">
      <div className="mb-4 flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold capitalize">{modality}</h1>
          <p className="text-xs text-muted-foreground">
            Configure the {modality} provider for cos agent.
          </p>
        </div>
        <div className="flex items-center gap-2 text-xs">
          {ready ? (
            <span className="flex items-center gap-1 text-emerald-500">
              <CheckCircle2 className="h-3.5 w-3.5" /> configured
            </span>
          ) : (
            <span className="flex items-center gap-1 text-yellow-500">
              <XCircle className="h-3.5 w-3.5" /> not configured
            </span>
          )}
        </div>
      </div>

      {loading ? (
        <div className="flex items-center gap-2 text-sm text-muted-foreground">
          <Loader2 className="h-4 w-4 animate-spin" />
          Loading…
        </div>
      ) : (
        <Card className="grid gap-4 p-5">
          <div className="grid gap-1.5">
            <Label>Provider</Label>
            <Select value={provider} onValueChange={setProvider}>
              <SelectTrigger>
                <SelectValue placeholder="Choose…" />
              </SelectTrigger>
              <SelectContent>
                {providers.map((p) => {
                  const id = providerId(p);
                  return (
                    <SelectItem key={id} value={id}>
                      {p.label || p.display || p.name || id}
                    </SelectItem>
                  );
                })}
              </SelectContent>
            </Select>
          </div>

          <div className="grid gap-1.5">
            <Label>Model</Label>
            {models.length > 0 ? (
              <Select value={model} onValueChange={setModel}>
                <SelectTrigger>
                  <SelectValue placeholder="Choose model…" />
                </SelectTrigger>
                <SelectContent>
                  {models.map((m) => (
                    <SelectItem key={m} value={m}>
                      {m}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            ) : (
              <Input
                placeholder="model id"
                value={model}
                onChange={(e) => setModel(e.target.value)}
              />
            )}
          </div>

          {needsBase && (
            <div className="grid gap-1.5">
              <Label>Base URL</Label>
              <Input
                placeholder="https://api.example.com/v1"
                value={baseUrl}
                onChange={(e) => setBaseUrl(e.target.value)}
              />
            </div>
          )}

          <div className="grid gap-1.5">
            <Label>API key</Label>
            <Input
              type="password"
              placeholder={
                providerEntry?.api_key_env
                  ? `from env ${providerEntry.api_key_env}, or paste`
                  : "Paste an API key"
              }
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
            />
            {supportsOauth && (
              <p className="text-[11px] text-muted-foreground">
                This provider supports OAuth. Use the button below instead of pasting a key.
              </p>
            )}
          </div>

          {msg && (
            <p
              className={cn(
                "text-sm",
                msg.kind === "ok" ? "text-emerald-500" : "text-destructive",
              )}
            >
              {msg.text}
            </p>
          )}

          <div className="flex flex-wrap gap-2">
            <Button disabled={busy != null || !provider} onClick={() => apply(true)}>
              {busy === "apply" ? (
                <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
              ) : null}
              Apply & verify
            </Button>
            <Button variant="outline" disabled={busy != null || !provider} onClick={() => apply(false)}>
              Apply
            </Button>
            <Button variant="outline" disabled={busy != null || !ready} onClick={test}>
              Verify
            </Button>
            {supportsOauth && (
              <Button variant="outline" disabled={busy != null || !provider} onClick={oauthStart}>
                OAuth sign-in
              </Button>
            )}
            <div className="ml-auto" />
            <Button
              variant="ghost"
              className="text-destructive"
              disabled={busy != null}
              onClick={reset}
            >
              <RotateCcw className="mr-1 h-3.5 w-3.5" />
              Reset
            </Button>
          </div>
        </Card>
      )}
    </div>
  );
}

function AboutPanel({ meta }: { meta: any }) {
  return (
    <div className="mx-auto max-w-2xl p-6">
      <h1 className="text-xl font-semibold">About</h1>
      <p className="mt-1 text-xs text-muted-foreground">
        cos agent web — running locally, served by{" "}
        <code className="font-mono">cos agent serve</code>.
      </p>
      <Card className="mt-4 p-5">
        <dl className="grid grid-cols-[120px_1fr] gap-y-2 text-sm">
          <dt className="text-muted-foreground">Version</dt>
          <dd className="font-mono">{meta?.version || "—"}</dd>
          <dt className="text-muted-foreground">Hostname</dt>
          <dd className="font-mono">{meta?.hostname || "—"}</dd>
          <dt className="text-muted-foreground">Provider</dt>
          <dd className="font-mono">{meta?.provider || "—"}</dd>
          <dt className="text-muted-foreground">Model</dt>
          <dd className="font-mono">{meta?.model || "—"}</dd>
        </dl>
      </Card>
    </div>
  );
}

function extractProviders(raw: any): ProviderEntry[] {
  if (!raw) return [];
  if (Array.isArray(raw)) return raw;
  if (Array.isArray(raw.providers)) return raw.providers;
  if (typeof raw === "object") {
    // Map-style { provider_id: meta }
    return Object.entries(raw).map(([k, v]) => ({
      id: k,
      ...(typeof v === "object" && v ? (v as object) : {}),
    }));
  }
  return [];
}

function extractModelIds(raw: any): string[] {
  if (!raw) return [];
  const list: any[] = Array.isArray(raw)
    ? raw
    : Array.isArray(raw.models)
      ? raw.models
      : [];
  return list.map((m) => (typeof m === "string" ? m : m?.id || m?.name || "")).filter(Boolean);
}
