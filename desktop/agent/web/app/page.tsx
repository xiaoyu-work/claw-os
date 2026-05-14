import { redirect as _redirect } from "next/navigation";
import { ChatShell } from "@/components/chat-shell";

void _redirect;

export default function Home() {
  return <ChatShell />;
}
