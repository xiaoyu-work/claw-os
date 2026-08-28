/**
 * Top-level shell. SidebarProvider + cos sidebar + main inset that
 * route-switches between pages. Same layout pattern as
 * `open-agents/apps/web/app/inbox-shell.tsx`.
 */

import { Bell } from "lucide-react";
import { useEffect, useState } from "react";

import { TokenGate } from "@/components/token-gate";
import { CosSidebar } from "@/components/cos-sidebar";
import { SidebarInset, SidebarProvider, SidebarTrigger } from "@/components/ui/sidebar";
import { Separator } from "@/components/ui/separator";
import { TooltipProvider } from "@/components/ui/tooltip";
import { NotificationProvider, useNotifications } from "@/lib/notifications";
import { navigate, useRoute } from "@/lib/router";

import { ChatPage } from "@/pages/chat";
import { TasksPage } from "@/pages/tasks";
import { ApprovalsPage } from "@/pages/approvals";
import { InboxPage } from "@/pages/inbox";
import { NotificationsPage } from "@/pages/notifications";
import { SystemPage } from "@/pages/system";
import { SettingsPage } from "@/pages/settings";

export default function App() {
  const [meta, setMeta] = useState<any>(null);

  return (
    <TooltipProvider delayDuration={250}>
      <TokenGate onMeta={setMeta}>
        <NotificationProvider>
          <SidebarProvider>
            <CosSidebar meta={meta} />
            <SidebarInset>
              <Header />
              <main className="flex-1 overflow-hidden">
                <Router meta={meta} />
              </main>
            </SidebarInset>
          </SidebarProvider>
        </NotificationProvider>
      </TokenGate>
    </TooltipProvider>
  );
}

function Header() {
  const route = useRoute();
  const { unreadCount } = useNotifications();
  const title = routeTitle(route);
  return (
    <header className="flex h-12 shrink-0 items-center gap-2 border-b px-3">
      <SidebarTrigger />
      <Separator orientation="vertical" className="mx-1 h-5" />
      <h1 className="text-sm font-medium">{title}</h1>
      <button
        type="button"
        className="relative ml-auto rounded-md p-2 hover:bg-muted"
        title="Notifications"
        onClick={() => navigate("/notifications")}
      >
        <Bell className="h-4 w-4" />
        {unreadCount > 0 && (
          <span className="absolute -right-0.5 -top-0.5 min-w-4 rounded-full bg-destructive px-1 text-center text-[10px] leading-4 text-destructive-foreground">
            {unreadCount > 99 ? "99+" : unreadCount}
          </span>
        )}
      </button>
    </header>
  );
}

function routeTitle(route: string): string {
  if (route.startsWith("/tasks")) return "Tasks";
  if (route.startsWith("/approvals")) return "Approvals";
  if (route.startsWith("/inbox")) return "Inbox";
  if (route.startsWith("/notifications")) return "Notifications";
  if (route.startsWith("/system")) return "System";
  if (route.startsWith("/settings")) return "Settings";
  return "Chat";
}

function Router({ meta }: { meta: any }) {
  const route = useRoute();
  if (route.startsWith("/tasks")) return <TasksPage />;
  if (route.startsWith("/approvals")) return <ApprovalsPage />;
  if (route.startsWith("/inbox")) return <InboxPage />;
  if (route.startsWith("/notifications")) return <NotificationsPage />;
  if (route.startsWith("/system")) return <SystemPage />;
  if (route.startsWith("/settings")) return <SettingsPage meta={meta} />;
  return <ChatPage meta={meta} />;
}
