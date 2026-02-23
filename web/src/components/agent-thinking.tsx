"use client";

import { useMemo, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { ToolCallCard } from "./tool-call-card";
import type { ToolCallInfo } from "./tool-call-card";
import type { RunActionEvent } from "@/lib/types";

/* ------------------------------------------------------------------ */
/*  Node timeline state                                                */
/* ------------------------------------------------------------------ */

interface NodeTimeline {
  nodeId: string;
  role: string | null;
  model: string | null;
  status: "active" | "completed" | "failed";
  tokens: string;
  toolCalls: ToolCallInfo[];
  startedAt: string | null;
}

const roleLabels: Record<string, string> = {
  planner: "Planner",
  extractor: "Extractor",
  coder: "Coder",
  summarizer: "Summarizer",
  fallback: "Fallback",
  tool_caller: "Tool Caller",
  analyzer: "Analyzer",
  reviewer: "Reviewer",
  scheduler: "Scheduler",
  config_manager: "Config",
};

const roleColors: Record<string, string> = {
  planner: "bg-blue-500",
  extractor: "bg-amber-500",
  coder: "bg-purple-500",
  summarizer: "bg-emerald-500",
  fallback: "bg-red-500",
  tool_caller: "bg-orange-500",
  analyzer: "bg-cyan-500",
  reviewer: "bg-pink-500",
  scheduler: "bg-indigo-500",
  config_manager: "bg-lime-500",
};

const mdClasses =
  "prose-sm max-w-none break-words text-slate-800 [&_a]:text-teal-600 [&_a]:underline [&_blockquote]:border-l-2 [&_blockquote]:border-slate-300 [&_blockquote]:pl-3 [&_blockquote]:text-slate-500 [&_code]:rounded [&_code]:bg-slate-200 [&_code]:px-1 [&_code]:py-0.5 [&_code]:text-xs [&_h1]:mb-2 [&_h1]:mt-3 [&_h1]:text-base [&_h1]:font-bold [&_h2]:mb-1.5 [&_h2]:mt-2 [&_h2]:text-sm [&_h2]:font-semibold [&_h3]:mb-1 [&_h3]:mt-2 [&_h3]:text-sm [&_h3]:font-medium [&_li]:ml-4 [&_ol]:list-decimal [&_ol]:pl-4 [&_p]:my-1.5 [&_pre>code]:block [&_pre>code]:bg-transparent [&_pre>code]:p-0 [&_pre]:my-2 [&_pre]:overflow-x-auto [&_pre]:rounded-lg [&_pre]:bg-slate-800 [&_pre]:p-3 [&_pre]:text-xs [&_pre]:text-slate-100 [&_table]:w-full [&_td]:border [&_td]:border-slate-200 [&_td]:px-2 [&_td]:py-1 [&_td]:text-xs [&_th]:border [&_th]:border-slate-200 [&_th]:bg-slate-100 [&_th]:px-2 [&_th]:py-1 [&_th]:text-xs [&_th]:font-medium [&_ul]:list-disc [&_ul]:pl-4";

function buildNodeTimeline(events: RunActionEvent[]): NodeTimeline[] {
  const nodes = new Map<string, NodeTimeline>();
  const order: string[] = [];

  for (const ev of events) {
    const p = ev.payload as Record<string, unknown>;

    if (ev.action === "node_started" && ev.actor_id) {
      if (!nodes.has(ev.actor_id)) {
        order.push(ev.actor_id);
        nodes.set(ev.actor_id, {
          nodeId: ev.actor_id,
          role: (p.role as string) ?? null,
          model: null,
          status: "active",
          tokens: "",
          toolCalls: [],
          startedAt: ev.timestamp,
        });
      }
    }

    if (ev.action === "node_token_chunk") {
      const nodeId = (p.node_id as string) ?? ev.actor_id ?? "";
      const token = (p.token as string) ?? "";
      const node = nodes.get(nodeId);
      if (node) node.tokens += token;
    }

    if (ev.action === "model_selected") {
      const nodeId = (p.node_id as string) ?? ev.actor_id ?? "";
      const model = (p.model as string) ?? (p.model_id as string) ?? null;
      const node = nodes.get(nodeId);
      if (node && model) node.model = model;
    }

    if (ev.action === "mcp_tool_called") {
      const nodeId = (p.node_id as string) ?? ev.actor_id ?? "";
      const node = nodes.get(nodeId);
      if (node) {
        node.toolCalls.push({
          tool_name: (p.tool_name as string) ?? "unknown",
          arguments: (p.arguments as Record<string, unknown>) ?? {},
          succeeded: p.succeeded as boolean | undefined,
          content: p.content as string | undefined,
          error: p.error as string | undefined,
          duration_ms: p.duration_ms as number | undefined,
          node_id: nodeId,
          timestamp: ev.timestamp,
        });
      }
    }

    if (ev.action === "node_completed" && ev.actor_id) {
      const node = nodes.get(ev.actor_id);
      if (node) node.status = "completed";
    }

    if (ev.action === "node_failed" && ev.actor_id) {
      const node = nodes.get(ev.actor_id);
      if (node) node.status = "failed";
    }
  }

  return order.map((id) => nodes.get(id)!);
}

/* ------------------------------------------------------------------ */
/*  Main component                                                     */
/* ------------------------------------------------------------------ */

interface Props {
  events: RunActionEvent[];
  isRunning: boolean;
}

export function AgentThinking({ events, isRunning }: Props) {
  const timeline = useMemo(() => buildNodeTimeline(events), [events]);

  if (timeline.length === 0) {
    if (!isRunning) return null;
    return (
      <div className="flex items-center gap-2 px-4 py-3">
        <BouncingDots />
        <span className="text-xs text-slate-400">Preparing...</span>
      </div>
    );
  }

  // Separate summarizer (final answer) from other nodes
  const stepNodes = timeline.filter((n) => n.role !== "summarizer");
  const summarizerNode = timeline.find((n) => n.role === "summarizer");

  const completedSteps = stepNodes.filter((n) => n.status !== "active");
  const activeSteps = stepNodes.filter((n) => n.status === "active");

  return (
    <div className="space-y-3">
      {/* Step cards (non-summarizer) */}
      <div className="space-y-2 px-4">
        {/* Completed steps → collapsed cards */}
        {completedSteps.map((node) => (
          <CompletedNodeCard key={node.nodeId} node={node} />
        ))}

        {/* Active steps → parallel if multiple */}
        {activeSteps.length > 1 ? (
          <div className="grid grid-cols-2 gap-3">
            {activeSteps.map((node) => (
              <ActiveNodePanel key={node.nodeId} node={node} />
            ))}
          </div>
        ) : activeSteps.length === 1 ? (
          <ActiveNodePanel node={activeSteps[0]} />
        ) : null}

        {/* Waiting for next step */}
        {isRunning && activeSteps.length === 0 && !summarizerNode && completedSteps.length > 0 && (
          <div className="flex items-center gap-2 py-1">
            <BouncingDots />
            <span className="text-xs text-slate-400">Next step...</span>
          </div>
        )}
      </div>

      {/* Summarizer = final answer (shown as full message, not collapsed card) */}
      {summarizerNode && (
        <SummarizerPanel node={summarizerNode} />
      )}
    </div>
  );
}

/* ------------------------------------------------------------------ */
/*  Completed node → collapsible card                                  */
/* ------------------------------------------------------------------ */

function CompletedNodeCard({ node }: { node: NodeTimeline }) {
  const [open, setOpen] = useState(false);
  const isFailed = node.status === "failed";
  const role = node.role ?? node.nodeId;
  const label = roleLabels[role] ?? role;
  const dotColor = isFailed ? "bg-red-500" : (roleColors[role] ?? "bg-slate-500");

  const preview = node.tokens
    ? node.tokens.split("\n").find((l) => l.trim().length > 0)?.trim().slice(0, 80) ?? ""
    : "";

  return (
    <div className="overflow-hidden rounded-lg border border-slate-200 bg-white shadow-sm">
      <button
        onClick={() => setOpen(!open)}
        className="flex w-full items-center gap-2 px-3 py-2 text-left transition-colors hover:bg-slate-50"
      >
        <svg
          className={`h-3 w-3 shrink-0 text-slate-400 transition-transform ${open ? "rotate-90" : ""}`}
          viewBox="0 0 12 12"
          fill="currentColor"
        >
          <path d="M4.5 2l4 4-4 4V2z" />
        </svg>

        <span className={`h-2 w-2 shrink-0 rounded-full ${dotColor}`} />
        <span className="text-xs font-medium text-slate-700">{label}</span>

        {node.model && (
          <span className="rounded bg-slate-100 px-1.5 py-0.5 text-[10px] text-slate-400">
            {node.model}
          </span>
        )}

        {isFailed ? (
          <span className="rounded bg-red-50 px-1.5 py-0.5 text-[10px] font-medium text-red-500">
            failed
          </span>
        ) : (
          <svg className="h-3.5 w-3.5 shrink-0 text-emerald-500" viewBox="0 0 16 16" fill="currentColor">
            <path d="M8 0a8 8 0 110 16A8 8 0 018 0zm3.78 5.22a.75.75 0 00-1.06 0L7 8.94 5.28 7.22a.75.75 0 10-1.06 1.06l2.25 2.25a.75.75 0 001.06 0l4.25-4.25a.75.75 0 000-1.06z" />
          </svg>
        )}

        {node.toolCalls.length > 0 && (
          <span className="rounded bg-violet-50 px-1.5 py-0.5 text-[10px] text-violet-500">
            {node.toolCalls.length} tool{node.toolCalls.length > 1 ? "s" : ""}
          </span>
        )}

        {!open && preview && (
          <span className="ml-auto truncate text-[10px] text-slate-400">
            {preview}
          </span>
        )}
      </button>

      {open && (
        <div className="border-t border-slate-100 px-3 py-3">
          {node.toolCalls.length > 0 && (
            <div className="mb-3 space-y-2">
              {node.toolCalls.map((tc, i) => (
                <ToolCallCard key={i} call={tc} />
              ))}
            </div>
          )}

          {node.tokens && (
            <div className={`max-h-64 overflow-y-auto text-xs ${mdClasses}`}>
              <ReactMarkdown remarkPlugins={[remarkGfm]}>
                {node.tokens}
              </ReactMarkdown>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/* ------------------------------------------------------------------ */
/*  Active node → streaming panel with markdown                        */
/* ------------------------------------------------------------------ */

function ActiveNodePanel({ node }: { node: NodeTimeline }) {
  const role = node.role ?? node.nodeId;
  const label = roleLabels[role] ?? role;
  const dotColor = roleColors[role] ?? "bg-slate-500";

  return (
    <div className="rounded-lg border border-slate-200 bg-slate-50 p-3">
      <div className="mb-2 flex items-center gap-2">
        <span className={`h-2 w-2 animate-pulse rounded-full ${dotColor}`} />
        <span className="text-xs font-medium text-slate-700">{label}</span>
        {node.model && (
          <span className="rounded bg-slate-100 px-1.5 py-0.5 text-[10px] text-slate-400">
            {node.model}
          </span>
        )}
        <BouncingDots />
      </div>

      {node.toolCalls.length > 0 && (
        <div className="mb-2 space-y-2">
          {node.toolCalls.map((tc, i) => (
            <ToolCallCard key={i} call={tc} />
          ))}
        </div>
      )}

      {node.tokens ? (
        <div className={`max-h-48 overflow-y-auto text-sm ${mdClasses}`}>
          <ReactMarkdown remarkPlugins={[remarkGfm]}>
            {node.tokens}
          </ReactMarkdown>
          <span className="animate-pulse text-teal-500">|</span>
        </div>
      ) : node.toolCalls.length === 0 ? (
        <span className="text-[10px] text-slate-400">Working...</span>
      ) : null}
    </div>
  );
}

/* ------------------------------------------------------------------ */
/*  Summarizer → final answer panel (always expanded)                  */
/* ------------------------------------------------------------------ */

function SummarizerPanel({ node }: { node: NodeTimeline }) {
  const isActive = node.status === "active";
  const isFailed = node.status === "failed";

  return (
    <div className="flex justify-start">
      {/* Summarizer avatar */}
      <div className="mr-2 mt-1 flex h-7 w-7 shrink-0 items-center justify-center rounded-full bg-emerald-100 text-[10px] font-bold text-emerald-700">
        S
      </div>
      <div className="min-w-0 max-w-[75%] rounded-xl border border-slate-200 bg-slate-50 px-4 py-2.5 text-sm">
        <div className="mb-1 flex items-center gap-1.5">
          <span className="inline-block rounded-full bg-emerald-100 px-2 py-0.5 text-[10px] font-medium text-emerald-700">
            summarizer
          </span>
          {node.model && (
            <span className="text-[10px] text-slate-400">{node.model}</span>
          )}
          {isActive && <BouncingDots />}
        </div>

        {isFailed ? (
          <div className="text-xs text-red-500">Summarization failed</div>
        ) : node.tokens ? (
          <div className={mdClasses}>
            <ReactMarkdown remarkPlugins={[remarkGfm]}>
              {node.tokens}
            </ReactMarkdown>
            {isActive && (
              <span className="animate-pulse text-teal-500">|</span>
            )}
          </div>
        ) : isActive ? (
          <span className="text-xs text-slate-400">Generating answer...</span>
        ) : null}
      </div>
    </div>
  );
}

/* ------------------------------------------------------------------ */
/*  Shared                                                             */
/* ------------------------------------------------------------------ */

function BouncingDots() {
  return (
    <div className="flex gap-0.5">
      <span className="h-1.5 w-1.5 animate-bounce rounded-full bg-teal-400 [animation-delay:0ms]" />
      <span className="h-1.5 w-1.5 animate-bounce rounded-full bg-teal-400 [animation-delay:150ms]" />
      <span className="h-1.5 w-1.5 animate-bounce rounded-full bg-teal-400 [animation-delay:300ms]" />
    </div>
  );
}
