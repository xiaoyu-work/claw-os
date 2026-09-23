/**
 * cos's primary navigation rail. Same structure as open-agents'
 * `inbox-sidebar.tsx`: SidebarHeader (brand + "New chat") +
 * SidebarContent (nav group, then date-grouped sessions group) +
 * SidebarFooter (status pill). The visual language is verbatim — we
 * keep the same shadcn Sidebar primitive and class names; only the
 * concepts being displayed are remapped to cos (no repos / branches /
 * GitHub OAuth — instead chat / tasks / approvals / inbox / settings,
 * plus a date-grouped session list from `/api/sessions`).
 */

import {
  Activity,
  Archive,
  ArchiveRestore,
  ChevronDown,
  GitFork,
  Inbox,
  ListTodo,
  MessageSquare,
  MoreHorizontal,
  Moon,
  Pencil,
  Plus,
  Search,
  ShieldCheck,
  Settings,
  Sun,
  Target,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";

import { api } from "@/lib/api";
import { useNotifications } from "@/lib/notifications";
import { isActive, navigate, useRoute } from "@/lib/router";
import { setTheme, useTheme, type Theme } from "@/lib/theme";
import { cn } from "@/lib/utils";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
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
  { key: "activities", label: "Activities", icon: Target, href: "/activities" },
  { key: "tasks", label: "Tasks", icon: ListTodo, href: "/tasks" },
  { key: "approvals", label: "Approvals", icon: ShieldCheck, href: "/approvals" },
  { key: "inbox", label: "Inbox", icon: Inbox, href: "/inbox" },
  { key: "events", label: "System Events", icon: Activity, href: "/events" },
  { key: "settings", label: "Settings", icon: Settings, href: "/settings" },
];

type Session = {
  id: string;
  presentation_id?: string;
  title?: string | null;
  preview?: string | null;
  updated_at?: number | string;
  created_at?: number | string;
  message_count?: number;
  archived?: boolean;
  parent_id?: string;
  manageable?: boolean;
  legacy?: boolean;
};

type SessionList = {
  sessions?: Session[];
  truncated?: boolean;
};

export function CosSidebar({ meta }: { meta: any }) {
  const current = useRoute();
  const { unreadCount } = useNotifications();
  const [sessions, setSessions] = useState<Session[]>([]);
  const [navOpen, setNavOpen] = useState(true);
  const [sessionsOpen, setSessionsOpen] = useState(true);
  const [showArchived, setShowArchived] = useState(false);
  const [sessionSearch, setSessionSearch] = useState("");
  const [sessionsTruncated, setSessionsTruncated] = useState(false);
  const [sessionError, setSessionError] = useState<string | null>(null);
  const [busySession, setBusySession] = useState<string | null>(null);
  const [renameSession, setRenameSession] = useState<Session | null>(null);
  const [renameTitle, setRenameTitle] = useState("");

  const fetchSessions = useCallback(async (archived: boolean) => {
    const suffix = archived ? "?archived=true" : "";
    const response = await api.get<SessionList | Session[]>(
      `/api/sessions${suffix}`,
    );
    return {
      sessions: Array.isArray(response) ? response : response?.sessions || [],
      truncated: Array.isArray(response) ? false : response?.truncated === true,
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    const refresh = () => {
      void fetchSessions(showArchived)
        .then((result) => {
          if (cancelled) return;
          setSessions(result.sessions);
          setSessionsTruncated(result.truncated);
          setSessionError(null);
        })
        .catch((error: any) => {
          if (!cancelled) {
            setSessionError(error?.message || "Failed to load conversations");
          }
        });
    };
    refresh();
    // The chat page dispatches `cos:sessions-changed` after the server
    // creates a fresh session id mid-stream, so the new chat appears in
    // the sidebar immediately rather than only after the next reload.
    const onChange = () => refresh();
    window.addEventListener("cos:sessions-changed", onChange);
    return () => {
      cancelled = true;
      window.removeEventListener("cos:sessions-changed", onChange);
    };
  }, [fetchSessions, showArchived]);

  const filteredSessions = useMemo(() => {
    const query = sessionSearch.trim().toLowerCase();
    if (!query) return sessions;
    return sessions.filter((session) =>
      `${session.title || ""} ${session.id}`.toLowerCase().includes(query),
    );
  }, [sessionSearch, sessions]);
  const grouped = useMemo(() => {
    const sorted = filteredSessions.slice().sort(
      (left, right) =>
        (parseTs(right.updated_at ?? right.created_at) || 0) -
        (parseTs(left.updated_at ?? left.created_at) || 0),
    );
    return groupByDate(sorted);
  }, [filteredSessions]);

  async function updateSession(
    session: Session,
    changes: { title?: string; archived?: boolean },
  ) {
    setBusySession(session.id);
    setSessionError(null);
    try {
      const response = await api.post<{ session: Session }>(
        `/api/sessions/${encodeURIComponent(session.id)}`,
        changes,
      );
      const updated = response.session;
      setSessions((current) => {
        if (!!updated.archived !== showArchived) {
          return current.filter((candidate) => candidate.id !== updated.id);
        }
        return current.map((candidate) =>
          candidate.id === updated.id ? updated : candidate,
        );
      });
      window.dispatchEvent(new CustomEvent("cos:sessions-changed"));
      return updated;
    } catch (error: any) {
      setSessionError(error?.message || "Failed to update conversation");
      return null;
    } finally {
      setBusySession(null);
    }
  }

  async function forkSession(session: Session) {
    setBusySession(session.id);
    setSessionError(null);
    try {
      const response = await api.post<{ session: Session }>(
        `/api/sessions/${encodeURIComponent(session.id)}/fork`,
      );
      setShowArchived(false);
      window.dispatchEvent(new CustomEvent("cos:sessions-changed"));
      navigate(`/chat/${response.session.id}`);
    } catch (error: any) {
      setSessionError(error?.message || "Failed to fork conversation");
    } finally {
      setBusySession(null);
    }
  }

  async function saveRename() {
    if (!renameSession) return;
    const title = renameTitle.trim();
    if (!title) return;
    const updated = await updateSession(renameSession, { title });
    if (updated) setRenameSession(null);
  }

  return (
    <Sidebar collapsible="offcanvas" className="border-r">
      <SidebarHeader className="gap-2">
        <div className="flex items-center justify-between px-2 py-1.5">
          <div className="flex items-center gap-2">
            <Logo />
            <div className="grid leading-tight">
              <span className="text-sm font-semibold tracking-tight">Claw OS</span>
              <span className="text-[11px] text-muted-foreground">
                {meta?.hostname || "localhost"}
              </span>
            </div>
          </div>
          <ThemeToggle />
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
                        {item.key === "inbox" && unreadCount > 0 && (
                          <span className="ml-auto min-w-5 rounded-full bg-destructive px-1 text-center text-[10px] leading-5 text-destructive-foreground">
                            {unreadCount > 99 ? "99+" : unreadCount}
                          </span>
                        )}
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
              <div className="grid gap-2 px-2 pb-2">
                <div className="relative">
                  <Search className="pointer-events-none absolute left-2.5 top-2.5 h-3.5 w-3.5 text-muted-foreground" />
                  <Input
                    aria-label="Search conversations"
                    value={sessionSearch}
                    onChange={(event) => setSessionSearch(event.target.value)}
                    placeholder="Search conversations"
                    className="h-8 pl-8 text-xs"
                  />
                </div>
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-7 justify-start gap-2 px-2 text-xs"
                  onClick={() => {
                    setShowArchived((value) => !value);
                    setSessionSearch("");
                  }}
                >
                  {showArchived ? (
                    <ArchiveRestore className="h-3.5 w-3.5" />
                  ) : (
                    <Archive className="h-3.5 w-3.5" />
                  )}
                  {showArchived ? "Active conversations" : "Archived conversations"}
                </Button>
                {sessionError && (
                  <p role="alert" className="text-xs text-destructive">
                    {sessionError}
                  </p>
                )}
                {sessionsTruncated && (
                  <p className="text-[11px] text-muted-foreground">
                    Showing the newest 1,000 conversations.
                  </p>
                )}
              </div>
              {grouped.length === 0 ? (
                <p className="px-3 py-2 text-[11px] text-muted-foreground">
                  {sessionSearch.trim()
                    ? "No matching conversations."
                    : showArchived
                      ? "No archived conversations."
                      : "No conversations yet — start a chat."}
                </p>
              ) : (
                grouped.map(([label, list]) => (
                  <div key={label} className="mb-2">
                    <div className="px-3 pb-1 pt-2 text-[10px] font-medium uppercase tracking-wider text-muted-foreground/70">
                      {label}
                    </div>
                    <SidebarMenu>
                      {list.map((s) => (
                        <SidebarMenuItem
                          key={s.id}
                          className="group/session flex items-center"
                        >
                          <SidebarMenuButton
                            isActive={current === `/chat/${s.id}`}
                            onClick={() => navigate(`/chat/${s.id}`)}
                            className="h-auto min-w-0 flex-1 py-1.5"
                            tooltip={s.title || s.id}
                          >
                            <span className="min-w-0 flex-1 truncate text-xs">
                              {s.title || s.preview || s.id.slice(0, 8)}
                            </span>
                            {s.legacy && (
                              <span
                                className="text-[9px] uppercase tracking-wide text-muted-foreground"
                                title="Previous conversation (read-only)"
                              >
                                read only
                              </span>
                            )}
                          </SidebarMenuButton>
                          {s.manageable !== false && (
                            <DropdownMenu>
                              <DropdownMenuTrigger asChild>
                                <button
                                  type="button"
                                  aria-label={`Manage conversation: ${s.title}`}
                                  className="mr-1 rounded p-1 text-muted-foreground opacity-0 hover:bg-sidebar-accent hover:text-sidebar-accent-foreground focus:opacity-100 group-hover/session:opacity-100"
                                >
                                  <MoreHorizontal className="h-3.5 w-3.5" />
                                </button>
                              </DropdownMenuTrigger>
                              <DropdownMenuContent side="right" align="start">
                                <DropdownMenuItem
                                  onSelect={() => {
                                    setRenameSession(s);
                                    setRenameTitle(s.title || "");
                                  }}
                                >
                                  <Pencil className="mr-2 h-3.5 w-3.5" />
                                  Rename
                                </DropdownMenuItem>
                                <DropdownMenuItem onSelect={() => void forkSession(s)}>
                                  <GitFork className="mr-2 h-3.5 w-3.5" />
                                  Fork conversation
                                </DropdownMenuItem>
                                <DropdownMenuSeparator />
                                <DropdownMenuItem
                                  onSelect={() =>
                                    void updateSession(s, {
                                      archived: !showArchived,
                                    })
                                  }
                                >
                                  {showArchived ? (
                                    <ArchiveRestore className="mr-2 h-3.5 w-3.5" />
                                  ) : (
                                    <Archive className="mr-2 h-3.5 w-3.5" />
                                  )}
                                  {showArchived ? "Restore to active" : "Archive"}
                                </DropdownMenuItem>
                              </DropdownMenuContent>
                            </DropdownMenu>
                          )}
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
      <Dialog
        open={renameSession !== null}
        onOpenChange={(open) => {
          if (!open && busySession === null) setRenameSession(null);
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Rename conversation</DialogTitle>
            <DialogDescription>
              This changes presentation metadata only. Conversation history and
              task identity stay unchanged.
            </DialogDescription>
          </DialogHeader>
          <label className="grid gap-2 text-sm">
            <span>Conversation title</span>
            <Input
              aria-label="Conversation title"
              value={renameTitle}
              onChange={(event) => setRenameTitle(event.target.value)}
              maxLength={128}
              autoFocus
            />
          </label>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setRenameSession(null)}
              disabled={busySession !== null}
            >
              Cancel
            </Button>
            <Button
              onClick={() => void saveRename()}
              disabled={!renameTitle.trim() || busySession !== null}
            >
              Save
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </Sidebar>
  );
}

function Logo() {
  // Brand symbol is dark strokes on light background; provide an inverted
  // variant for dark mode so the strokes stay visible. Both PNGs live in
  // public/ so Vite serves them at root.
  return (
    <span className="relative grid h-7 w-7 place-items-center">
      <img
        src="/clawos-symbol.png"
        alt=""
        className="h-6 w-6 object-contain dark:hidden"
      />
      <img
        src="/clawos-symbol-dark.png"
        alt=""
        className="hidden h-6 w-6 object-contain dark:block"
      />
    </span>
  );
}

function ThemeToggle() {
  const theme = useTheme();
  // Collapse "system" to whichever colour scheme it is currently
  // resolving to. A single click then unambiguously flips the visible
  // theme — no hidden tri-state surprises.
  const effective: "dark" | "light" =
    theme === "system"
      ? (typeof window !== "undefined" &&
        window.matchMedia("(prefers-color-scheme: dark)").matches
          ? "dark"
          : "light")
      : theme;
  const next: Theme = effective === "dark" ? "light" : "dark";
  const Icon = effective === "dark" ? Sun : Moon;
  const label = effective === "dark" ? "Switch to light mode" : "Switch to dark mode";
  // Deliberately a plain Button — no Radix Tooltip / DropdownMenu
  // wrapping. The previous implementation nested a Tooltip around a
  // DropdownMenuTrigger via two layers of `asChild`, and Tooltip's
  // pointer-event handlers ate the click so the menu never opened.
  // A direct `onClick` toggle is simpler, more discoverable, and
  // cannot suffer the same bug.
  return (
    <Button
      variant="ghost"
      size="icon"
      className="h-7 w-7"
      aria-label={label}
      title={label}
      onClick={() => setTheme(next)}
    >
      <Icon className="h-3.5 w-3.5" />
    </Button>
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
