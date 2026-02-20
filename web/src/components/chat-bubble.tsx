"use client";

import type { ChatMessage } from "@/lib/types";

const roleColors: Record<string, string> = {
  planner: "bg-blue-100 text-blue-700",
  extractor: "bg-amber-100 text-amber-700",
  coder: "bg-purple-100 text-purple-700",
  summarizer: "bg-emerald-100 text-emerald-700",
  fallback: "bg-red-100 text-red-700",
  tool_caller: "bg-orange-100 text-orange-700",
};

export function ChatBubble({ message }: { message: ChatMessage }) {
  const isUser = message.role === "user";
  const isSystem = message.role === "system";

  if (isSystem) {
    return (
      <div className="flex justify-center py-1">
        <span className="rounded-full bg-slate-100 px-3 py-1 text-xs text-slate-400">
          {message.content}
        </span>
      </div>
    );
  }

  return (
    <div className={`flex ${isUser ? "justify-end" : "justify-start"}`}>
      <div
        className={`max-w-[75%] rounded-xl px-4 py-2.5 text-sm ${
          isUser
            ? "bg-teal-600 text-white"
            : "border border-slate-200 bg-slate-50 text-slate-800"
        }`}
      >
        {!isUser && (
          <div className="mb-1 flex items-center gap-1.5">
            {message.agent_role && (
              <span
                className={`inline-block rounded-full px-2 py-0.5 text-[10px] font-medium ${
                  roleColors[message.agent_role] ?? "bg-slate-100 text-slate-600"
                }`}
              >
                {message.agent_role}
              </span>
            )}
            {message.model && (
              <span className="text-[10px] text-slate-400">
                {message.model}
              </span>
            )}
          </div>
        )}
        <div className="whitespace-pre-wrap break-words">{message.content}</div>
        <div
          className={`mt-1 text-[10px] ${
            isUser ? "text-teal-200" : "text-slate-400"
          }`}
        >
          {new Date(message.timestamp).toLocaleTimeString()}
        </div>
      </div>
    </div>
  );
}
