"use client";

import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { ChatMessage } from "@/lib/types";

const roleColors: Record<string, string> = {
  planner: "bg-blue-100 text-blue-700",
  extractor: "bg-amber-100 text-amber-700",
  coder: "bg-purple-100 text-purple-700",
  summarizer: "bg-emerald-100 text-emerald-700",
  fallback: "bg-red-100 text-red-700",
  tool_caller: "bg-orange-100 text-orange-700",
  analyzer: "bg-cyan-100 text-cyan-700",
  reviewer: "bg-pink-100 text-pink-700",
  scheduler: "bg-indigo-100 text-indigo-700",
  config_manager: "bg-lime-100 text-lime-700",
};

const roleIcons: Record<string, string> = {
  planner: "P",
  extractor: "E",
  coder: "</>",
  summarizer: "S",
  fallback: "!",
  tool_caller: "T",
  analyzer: "A",
  reviewer: "R",
  scheduler: "C",
  config_manager: "G",
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

  const roleBg = message.agent_role
    ? roleColors[message.agent_role] ?? "bg-slate-100 text-slate-600"
    : "";
  const roleIcon = message.agent_role
    ? roleIcons[message.agent_role] ?? "?"
    : "";

  return (
    <div className={`flex ${isUser ? "justify-end" : "justify-start"}`}>
      {/* Agent avatar */}
      {!isUser && message.agent_role && (
        <div
          className={`mr-2 mt-1 flex h-7 w-7 shrink-0 items-center justify-center rounded-full text-[10px] font-bold ${roleBg}`}
        >
          {roleIcon}
        </div>
      )}
      <div
        className={`min-w-0 max-w-[75%] rounded-xl px-4 py-2.5 text-sm ${
          isUser
            ? "bg-teal-600 text-white"
            : "border border-slate-200 bg-slate-50 text-slate-800"
        }`}
      >
        {!isUser && (
          <div className="mb-1 flex items-center gap-1.5">
            {message.agent_role && (
              <span
                className={`inline-block rounded-full px-2 py-0.5 text-[10px] font-medium ${roleBg}`}
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
        {isUser ? (
          <div className="whitespace-pre-wrap break-words">
            {message.content}
          </div>
        ) : (
          <div className="prose-sm max-w-none break-words text-slate-800 [&_a]:text-teal-600 [&_a]:underline [&_blockquote]:border-l-2 [&_blockquote]:border-slate-300 [&_blockquote]:pl-3 [&_blockquote]:text-slate-500 [&_code]:rounded [&_code]:bg-slate-200 [&_code]:px-1 [&_code]:py-0.5 [&_code]:text-xs [&_h1]:mb-2 [&_h1]:mt-3 [&_h1]:text-base [&_h1]:font-bold [&_h2]:mb-1.5 [&_h2]:mt-2 [&_h2]:text-sm [&_h2]:font-semibold [&_h3]:mb-1 [&_h3]:mt-2 [&_h3]:text-sm [&_h3]:font-medium [&_li]:ml-4 [&_ol]:list-decimal [&_ol]:pl-4 [&_p]:my-1.5 [&_pre>code]:block [&_pre>code]:bg-transparent [&_pre>code]:p-0 [&_pre]:my-2 [&_pre]:overflow-x-auto [&_pre]:rounded-lg [&_pre]:bg-slate-800 [&_pre]:p-3 [&_pre]:text-xs [&_pre]:text-slate-100 [&_table]:w-full [&_td]:border [&_td]:border-slate-200 [&_td]:px-2 [&_td]:py-1 [&_td]:text-xs [&_th]:border [&_th]:border-slate-200 [&_th]:bg-slate-100 [&_th]:px-2 [&_th]:py-1 [&_th]:text-xs [&_th]:font-medium [&_ul]:list-disc [&_ul]:pl-4">
            <ReactMarkdown remarkPlugins={[remarkGfm]}>
              {message.content}
            </ReactMarkdown>
          </div>
        )}
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
