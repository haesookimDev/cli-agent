"use client";

import { Suspense } from "react";
import { ChatContent } from "@/components/chat/chat-view";

export default function TeamChatPage() {
  return (
    <Suspense
      fallback={
        <div className="p-6 text-center text-sm text-slate-400">Loading...</div>
      }
    >
      <ChatContent mode="team" />
    </Suspense>
  );
}
