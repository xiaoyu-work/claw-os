/**
 * Top-level shell. SidebarProvider + cos sidebar + main inset that
 * route-switches between pages. Same layout pattern as
 * `open-agents/apps/web/app/inbox-shell.tsx`.
 */

import { useEffect, useState } from "react";

import { TokenGate } from "@/components/token-gate";
import { CosSidebar } from "@/components/cos-sidebar";
import { SidebarInset, SidebarProvider, SidebarTrigger } from "@/components/ui/sidebar";
import { Separator } from "@/components/ui/separator";
import { TooltipProvider } from "@/components/ui/tooltip";
import { useRoute } from "@/lib/router";

import { ChatPage } from "@/pages/chat";
import { TasksPage } from "@/pages/tasks";
import { ApprovalsPage } from "@/pages/approvals";
import { InboxPage } from "@/pages/inbox";
import { SystemPage } from "@/pages/system";
import { SettingsPage } from "@/pages/settings";

export default function App() {
  const [meta, setMeta] = useState<any>(null);

  return (
    <TooltipProvider delayDuration={250}>
      <TokenGate onMeta={setMeta}>
        <SidebarProvider>
          <CosSidebar meta={meta} />
          <SidebarInset>
            <Header />
            <main className="flex-1 overflow-hidden">
              <Router meta={meta} />
            </main>
          </SidebarInset>
        </SidebarProvider>
      </TokenGate>
    </TooltipProvider>
  );
}

function Header() {
  const route = useRoute();
  const title = routeTitle(route);
  return (
    <header className="flex h-12 shrink-0 items-center gap-2 border-b px-3">
      <SidebarTrigger />
      <Separator orientation="vertical" className="mx-1 h-5" />
      <h1 className="text-sm font-medium">{title}</h1>
    </header>
  );
}

function routeTitle(route: string): string {
  if (route.startsWith("/tasks")) return "Tasks";
  if (route.startsWith("/approvals")) return "Approvals";
  if (route.startsWith("/inbox")) return "Inbox";
  if (route.startsWith("/system")) return "System";
  if (route.startsWith("/settings")) return "Settings";
  return "Chat";
}

function Router({ meta }: { meta: any }) {
  const route = useRoute();
  if (route.startsWith("/tasks")) return <TasksPage />;
  if (route.startsWith("/approvals")) return <ApprovalsPage />;
  if (route.startsWith("/inbox")) return <InboxPage />;
  if (route.startsWith("/system")) return <SystemPage />;
  if (route.startsWith("/settings")) return <SettingsPage meta={meta} />;
  return <ChatPage meta={meta} />;
}
