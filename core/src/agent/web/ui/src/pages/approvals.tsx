import { ApprovalViews } from "@/components/chat-approvals";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";

export function ApprovalsPage() {
  return (
    <div className="flex h-full flex-col gap-4 overflow-y-auto p-6">
      <h1 className="text-xl font-semibold">Approvals</h1>
      <Tabs defaultValue="pending">
        <TabsList><TabsTrigger value="pending">Pending</TabsTrigger><TabsTrigger value="recent">Recent</TabsTrigger></TabsList>
        <TabsContent value="pending"><ApprovalViews /></TabsContent>
        <TabsContent value="recent"><ApprovalViews recent /></TabsContent>
      </Tabs>
    </div>
  );
}
