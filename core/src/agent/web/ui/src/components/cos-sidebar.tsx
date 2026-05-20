/**
 * cos's primary navigation rail. Same structure as open-agents'
 * `inbox-sidebar.tsx`: SidebarHeader (brand + "New chat") +
 * SidebarContent (nav group, then date-grouped sessions group) +
 * SidebarFooter (status pill). The visual language is verbatim — we
 * keep the same shadcn Sidebar primitive and class names; only the
 * concepts being displayed are remapped to cos (no repos / branches /
 * GitHub OAuth — instead chat / tasks / approvals / inbox / system /
 * settings, plus a date-grouped session list from `/api/sessions`).
 */

import {
  ChevronDown,
  Inbox,
  ListTodo,
  MessageSquare,
  Monitor,
  Plus,
  ShieldCheck,
  Settings,
} from "lucide-react";
import { useEffect, useMemo, useState } from "react";

import { api } from "@/lib/api";
import { isActive, navigate, useRoute } from "@/lib/router";
import { cn } from "@/lib/utils";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
} from "@/components/ui/sidebar";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

const NAV_ITEMS: Array<{
  key: string;
  label: string;
  icon: typeof MessageSquare;
  href: string;
}> = [
  { key: "chat", label: "Chat", icon: MessageSquare, href: "/chat" },
  { key: "tasks", label: "Tasks", icon: ListTodo, href: "/tasks" },
  { key: "approvals", label: "Approvals", icon: ShieldCheck, href: "/approvals" },
  { key: "inbox", label: "Inbox", icon: Inbox, href: "/inbox" },
  { key: "system", label: "System", icon: Monitor, href: "/system" },
  { key: "settings", label: "Settings", icon: Settings, href: "/settings" },
];

type Session = {
  id: string;
  title?: string | null;
  preview?: string | null;
  updated_at?: number | string;
  created_at?: number | string;
  message_count?: number;
};

export function CosSidebar({ meta }: { meta: any }) {
  const current = useRoute();
  const [sessions, setSessions] = useState<Session[]>([]);
  const [navOpen, setNavOpen] = useState(true);
  const [sessionsOpen, setSessionsOpen] = useState(true);

  useEffect(() => {
    let cancelled = false;
    api
      .get<{ sessions?: Session[] } | Session[]>("/api/sessions")
      .then((r) => {
        if (cancelled) return;
        const list = Array.isArray(r) ? r : r?.sessions || [];
        setSessions(list);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  const grouped = useMemo(() => groupByDate(sessions), [sessions]);

  return (
    <Sidebar collapsible="offcanvas" className="border-r">
      <SidebarHeader className="gap-2">
        <div className="flex items-center justify-between px-2 py-1.5">
          <div className="flex items-center gap-2">
            <div className="grid h-7 w-7 place-items-center rounded-md bg-sidebar-primary text-sidebar-primary-foreground">
              <MessageSquare className="h-4 w-4" />
            </div>
            <div className="grid leading-tight">
              <span className="text-sm font-semibold tracking-tight">cos agent</span>
              <span className="text-[11px] text-muted-foreground">
                {meta?.hostname || "localhost"}
              </span>
            </div>
          </div>
        </div>
        <Button
          variant="default"
          size="sm"
          className="mx-2 h-8 justify-start gap-2 text-xs font-medium"
          onClick={() => navigate("/chat")}
        >
          <Plus className="h-3.5 w-3.5" />
          New chat
        </Button>
      </SidebarHeader>

      <SidebarContent>
        <SidebarGroup>
          <button
            type="button"
            onClick={() => setNavOpen((v) => !v)}
            className="flex w-full items-center gap-1.5 px-2 text-xs text-sidebar-foreground/70 hover:text-sidebar-foreground"
          >
            <ChevronDown
              className={cn("h-3.5 w-3.5 transition-transform", navOpen ? "" : "-rotate-90")}
            />
            <SidebarGroupLabel className="px-0">Navigation</SidebarGroupLabel>
          </button>
          {navOpen && (
            <SidebarGroupContent>
              <SidebarMenu>
                {NAV_ITEMS.map((item) => {
                  const active = isActive(item.href, current);
                  const Icon = item.icon;
                  return (
                    <SidebarMenuItem key={item.key}>
                      <SidebarMenuButton
                        isActive={active}
                        onClick={() => navigate(item.href)}
                        tooltip={item.label}
                      >
                        <Icon className="h-4 w-4" />
                        <span>{item.label}</span>
                      </SidebarMenuButton>
                    </SidebarMenuItem>
                  );
                })}
              </SidebarMenu>
            </SidebarGroupContent>
          )}
        </SidebarGroup>

        <SidebarGroup>
          <button
            type="button"
            onClick={() => setSessionsOpen((v) => !v)}
            className="flex w-full items-center gap-1.5 px-2 text-xs text-sidebar-foreground/70 hover:text-sidebar-foreground"
          >
            <ChevronDown
              className={cn("h-3.5 w-3.5 transition-transform", sessionsOpen ? "" : "-rotate-90")}
            />
            <SidebarGroupLabel className="px-0">
              Sessions{sessions.length ? ` (${sessions.length})` : ""}
            </SidebarGroupLabel>
          </button>
          {sessionsOpen && (
            <SidebarGroupContent>
              {grouped.length === 0 ? (
                <p className="px-3 py-2 text-[11px] text-muted-foreground">
                  No sessions yet — start a chat.
                </p>
              ) : (
                grouped.map(([label, list]) => (
                  <div key={label} className="mb-2">
                    <div className="px-3 pb-1 pt-2 text-[10px] font-medium uppercase tracking-wider text-muted-foreground/70">
                      {label}
                    </div>
                    <SidebarMenu>
                      {list.map((s) => (
                        <SidebarMenuItem key={s.id}>
                          <SidebarMenuButton
                            isActive={current === `/chat/${s.id}`}
                            onClick={() => navigate(`/chat/${s.id}`)}
                            className="h-auto py-1.5"
                            tooltip={s.title || s.id}
                          >
                            <span className="truncate text-xs">
                              {s.title || s.preview || s.id.slice(0, 8)}
                            </span>
                          </SidebarMenuButton>
                        </SidebarMenuItem>
                      ))}
                    </SidebarMenu>
                  </div>
                ))
              )}
            </SidebarGroupContent>
          )}
        </SidebarGroup>
      </SidebarContent>

      <SidebarFooter>
        <SidebarFooterUser meta={meta} />
      </SidebarFooter>
    </Sidebar>
  );
}

function SidebarFooterUser({ meta }: { meta: any }) {
  const provider = meta?.provider || "unconfigured";
  const model = meta?.model || "";
  const ready = provider && provider !== "mock" && provider !== "unconfigured";
  return (
    <DropdownMenu>
      <Tooltip>
        <TooltipTrigger asChild>
          <DropdownMenuTrigger asChild>
            <button
              type="button"
              className="mx-1 my-1 flex items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm hover:bg-sidebar-accent hover:text-sidebar-accent-foreground focus:outline-none"
            >
              <Avatar className="h-7 w-7">
                <AvatarFallback className="bg-sidebar-primary text-sidebar-primary-foreground text-xs">
                  {(meta?.hostname || "C").slice(0, 1).toUpperCase()}
                </AvatarFallback>
              </Avatar>
              <div className="grid flex-1 leading-tight">
                <span className="truncate text-xs font-medium">
                  {meta?.hostname || "local"}
                </span>
                <span className="flex items-center gap-1 text-[10px] text-muted-foreground">
                  <span
                    className={cn(
                      "h-1.5 w-1.5 rounded-full",
                      ready ? "bg-emerald-500" : "bg-yellow-500",
                    )}
                  />
                  <span className="truncate">
                    {provider}
                    {model ? ` · ${model}` : ""}
                  </span>
                </span>
              </div>
            </button>
          </DropdownMenuTrigger>
        </TooltipTrigger>
        <TooltipContent side="right">Settings & sign-out</TooltipContent>
      </Tooltip>
      <DropdownMenuContent side="right" align="end" className="w-56">
        <DropdownMenuLabel className="text-xs text-muted-foreground">
          {meta?.provider || "unconfigured"}
          {meta?.model ? ` · ${meta.model}` : ""}
        </DropdownMenuLabel>
        <DropdownMenuSeparator />
        <DropdownMenuItem onClick={() => navigate("/settings")}>
          <Settings className="mr-2 h-3.5 w-3.5" />
          Settings
        </DropdownMenuItem>
        <DropdownMenuItem onClick={() => navigate("/settings/about")}>
          About
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem
          className="text-destructive focus:text-destructive"
          onClick={() => {
            try {
              localStorage.removeItem("cos.token");
            } catch {}
            location.reload();
          }}
        >
          Sign out
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function groupByDate(sessions: Session[]): Array<[string, Session[]]> {
  const now = new Date();
  const today = startOfDay(now);
  const yesterday = new Date(today);
  yesterday.setDate(yesterday.getDate() - 1);
  const sevenDaysAgo = new Date(today);
  sevenDaysAgo.setDate(sevenDaysAgo.getDate() - 7);

  const buckets: Record<string, Session[]> = {
    Today: [],
    Yesterday: [],
    "Last 7 days": [],
    Older: [],
  };

  for (const s of sessions) {
    const ts = parseTs(s.updated_at ?? s.created_at);
    const d = ts ? new Date(ts) : null;
    if (!d) {
      buckets.Older.push(s);
    } else if (d >= today) {
      buckets.Today.push(s);
    } else if (d >= yesterday) {
      buckets.Yesterday.push(s);
    } else if (d >= sevenDaysAgo) {
      buckets["Last 7 days"].push(s);
    } else {
      buckets.Older.push(s);
    }
  }

  return Object.entries(buckets).filter(([, list]) => list.length > 0);
}

function startOfDay(d: Date) {
  const c = new Date(d);
  c.setHours(0, 0, 0, 0);
  return c;
}

function parseTs(v: number | string | undefined): number | null {
  if (v == null) return null;
  if (typeof v === "number") return v < 1e12 ? v * 1000 : v;
  const n = Number(v);
  if (!isNaN(n)) return n < 1e12 ? n * 1000 : n;
  const t = Date.parse(v);
  return isNaN(t) ? null : t;
}
