"use client";

import { useMemo, useState } from "react";
import type { RunActionEvent } from "@/lib/types";

interface ToolCallInfo {
  tool_name: string;
  arguments: Record<string, unknown>;
  succeeded?: boolean;
  content?: string;
  error?: string;
  duration_ms?: number;
  node_id?: string;
  timestamp: string;
}

interface Props {
  events: RunActionEvent[];
  isRunning: boolean;
}

export function AgentThinking({ events, isRunning }: Props) {
  if (!isRunning) return null;

  const started = new Set<string>();
  const finished = new Set<string>();
  let currentModel: string | null = null;

  for (const ev of events) {
    if (ev.action === "node_started" && ev.actor_id) {
      started.add(ev.actor_id);
    }
    if (
      (ev.action === "node_completed" || ev.action === "node_failed") &&
      ev.actor_id
    ) {
      finished.add(ev.actor_id);
    }
    if (ev.action === "model_selected") {
      currentModel =
        (ev.payload as Record<string, unknown>).model_id as string ?? null;
    }
  }

  const activeNodes = [...started].filter((n) => !finished.has(n));

  const streamingTokens = useMemo(() => {
    const tokens: Record<string, string> = {};
    for (const ev of events) {
      if (ev.action === "node_token_chunk") {
        const p = ev.payload as Record<string, unknown>;
        const nodeId = (p.node_id as string) ?? ev.actor_id ?? "";
        const token = (p.token as string) ?? "";
        if (nodeId && token) {
          tokens[nodeId] = (tokens[nodeId] ?? "") + token;
        }
      }
    }
    return tokens;
  }, [events]);

  const toolCalls = useMemo(() => {
    const calls: ToolCallInfo[] = [];
    for (const ev of events) {
      if (ev.action === "mcp_tool_called") {
        const p = ev.payload as Record<string, unknown>;
        calls.push({
          tool_name: (p.tool_name as string) ?? "unknown",
          arguments: (p.arguments as Record<string, unknown>) ?? {},
          succeeded: p.succeeded as boolean | undefined,
          content: p.content as string | undefined,
          error: p.error as string | undefined,
          duration_ms: p.duration_ms as number | undefined,
          node_id: (p.node_id as string) ?? ev.actor_id ?? undefined,
          timestamp: ev.timestamp,
        });
      }
    }
    return calls;
  }, [events]);

  if (activeNodes.length === 0 && toolCalls.length === 0) {
    return (
      <div className="flex items-center gap-2 px-4 py-3">
        <div className="flex gap-1">
          <span className="h-2 w-2 animate-bounce rounded-full bg-teal-400 [animation-delay:0ms]" />
          <span className="h-2 w-2 animate-bounce rounded-full bg-teal-400 [animation-delay:150ms]" />
          <span className="h-2 w-2 animate-bounce rounded-full bg-teal-400 [animation-delay:300ms]" />
        </div>
        <span className="text-xs text-slate-400">Preparing...</span>
      </div>
    );
  }

  const hasStreamingContent = activeNodes.some((n) => streamingTokens[n]);

  return (
    <div className="space-y-2 px-4 py-3">
      {/* Status bar */}
      {activeNodes.length > 0 && (
        <div className="flex items-center gap-2">
          <div className="flex gap-1">
            <span className="h-2 w-2 animate-bounce rounded-full bg-teal-400 [animation-delay:0ms]" />
            <span className="h-2 w-2 animate-bounce rounded-full bg-teal-400 [animation-delay:150ms]" />
            <span className="h-2 w-2 animate-bounce rounded-full bg-teal-400 [animation-delay:300ms]" />
          </div>
          <span className="text-xs text-slate-500">
            {activeNodes.map((n) => (
              <span
                key={n}
                className="mr-1.5 inline-block rounded bg-slate-100 px-1.5 py-0.5 font-mono text-[10px] text-slate-600"
              >
                {n}
              </span>
            ))}
            {currentModel && (
              <span className="text-slate-400">using {currentModel}</span>
            )}
          </span>
        </div>
      )}

      {/* Tool call cards */}
      {toolCalls.length > 0 && (
        <div className="space-y-2">
          {toolCalls.map((tc, i) => (
            <ToolCallCard key={i} call={tc} />
          ))}
        </div>
      )}

      {/* Streaming output */}
      {hasStreamingContent && (
        <div className="space-y-1.5">
          {activeNodes
            .filter((n) => streamingTokens[n])
            .map((nodeId) => (
              <div
                key={nodeId}
                className="rounded-lg border border-slate-100 bg-slate-50 p-3"
              >
                <div className="mb-1 text-[10px] font-semibold uppercase tracking-wider text-slate-400">
                  {nodeId}
                </div>
                <pre className="whitespace-pre-wrap break-words text-xs text-slate-700">
                  {streamingTokens[nodeId]}
                  <span className="animate-pulse text-teal-500">|</span>
                </pre>
              </div>
            ))}
        </div>
      )}
    </div>
  );
}

/** Format tool arguments as a concise one-line summary */
function argsSummary(args: Record<string, unknown>): string {
  if (!args) return "";
  const entries = Object.entries(args);
  if (entries.length === 0) return "";
  const parts = entries.slice(0, 3).map(([k, v]) => {
    const val = typeof v === "string"
      ? (v.length > 40 ? v.slice(0, 40) + "..." : v)
      : JSON.stringify(v);
    return `${k}: ${val}`;
  });
  if (entries.length > 3) parts.push("...");
  return parts.join(", ");
}

function ToolCallCard({ call }: { call: ToolCallInfo }) {
  const [open, setOpen] = useState(false);
  const ok = call.succeeded === true;
  const failed = call.succeeded === false;
  const pending = call.succeeded === undefined;

  // Extract short tool name (after server prefix)
  const shortName = call.tool_name.includes("/")
    ? call.tool_name.split("/").pop()!
    : call.tool_name;
  const serverPrefix = call.tool_name.includes("/")
    ? call.tool_name.split("/")[0]
    : null;

  return (
    <div className="overflow-hidden rounded-lg bg-slate-800 shadow-sm">
      {/* Header bar — always visible, clickable */}
      <button
        onClick={() => setOpen(!open)}
        className="flex w-full items-center gap-2 px-3 py-2.5 text-left transition-colors hover:bg-slate-700/50"
      >
        {/* Expand chevron */}
        <svg
          className={`h-3 w-3 shrink-0 text-slate-500 transition-transform ${open ? "rotate-90" : ""}`}
          viewBox="0 0 12 12"
          fill="currentColor"
        >
          <path d="M4.5 2l4 4-4 4V2z" />
        </svg>

        {/* Tool icon */}
        <span className="text-sm">
          {pending ? "\u26A1" : ok ? "" : ""}
        </span>

        {/* Tool call display */}
        <span className="flex-1 truncate font-mono text-xs text-slate-200">
          <span className="text-slate-500">$ </span>
          {shortName}
          {argsSummary(call.arguments) && (
            <span className="ml-1.5 text-slate-400">
              {"{" + argsSummary(call.arguments) + "}"}
            </span>
          )}
        </span>

        {/* Right side: tag + status */}
        <span className="shrink-0 rounded bg-slate-700 px-1.5 py-0.5 font-mono text-[10px] text-slate-400">
          {serverPrefix ?? "mcp"}
        </span>
        {pending ? (
          <span className="h-3 w-3 shrink-0 animate-spin rounded-full border border-slate-500 border-t-teal-400" />
        ) : ok ? (
          <svg className="h-3.5 w-3.5 shrink-0 text-emerald-400" viewBox="0 0 16 16" fill="currentColor">
            <path d="M8 0a8 8 0 110 16A8 8 0 018 0zm3.78 5.22a.75.75 0 00-1.06 0L7 8.94 5.28 7.22a.75.75 0 10-1.06 1.06l2.25 2.25a.75.75 0 001.06 0l4.25-4.25a.75.75 0 000-1.06z" />
          </svg>
        ) : (
          <svg className="h-3.5 w-3.5 shrink-0 text-red-400" viewBox="0 0 16 16" fill="currentColor">
            <path d="M8 0a8 8 0 110 16A8 8 0 018 0zm2.78 4.22a.75.75 0 00-1.06 0L8 5.94 6.28 4.22a.75.75 0 10-1.06 1.06L6.94 7 5.22 8.72a.75.75 0 101.06 1.06L8 8.06l1.72 1.72a.75.75 0 101.06-1.06L9.06 7l1.72-1.72a.75.75 0 000-1.06z" />
          </svg>
        )}
      </button>

      {/* Expanded detail panel */}
      {open && (
        <div className="border-t border-slate-700 bg-slate-900 px-3 py-3">
          {/* Meta row */}
          <div className="mb-2 flex items-center gap-3 text-[10px] text-slate-500">
            {call.duration_ms != null && (
              <span>
                <span className="mr-0.5">{"\u23F1"}</span>
                {String(call.duration_ms)}ms
              </span>
            )}
            {call.node_id && (
              <span>
                node: <span className="text-slate-400">{call.node_id}</span>
              </span>
            )}
          </div>

          {/* Arguments */}
          {Object.keys(call.arguments).length > 0 && (
            <div className="mb-2">
              <div className="mb-1 text-[10px] font-semibold uppercase tracking-wider text-slate-500">
                Arguments
              </div>
              <pre className="max-h-32 overflow-y-auto whitespace-pre-wrap rounded bg-slate-800 p-2 font-mono text-[11px] text-slate-300">
                {JSON.stringify(call.arguments, null, 2)}
              </pre>
            </div>
          )}

          {/* Result / Error */}
          {(call.content || call.error) && (
            <div>
              <div className="mb-1 text-[10px] font-semibold uppercase tracking-wider text-slate-500">
                {failed ? "Error" : "Output"}
              </div>
              <pre
                className={`max-h-64 overflow-y-auto whitespace-pre-wrap rounded p-2 font-mono text-[11px] ${
                  failed
                    ? "bg-red-950/40 text-red-300"
                    : "bg-slate-800 text-slate-300"
                }`}
              >
                {call.error ?? call.content}
              </pre>
            </div>
          )}

          {/* Footer status */}
          <div className="mt-2 flex items-center gap-1.5 text-[10px]">
            {ok && <span className="text-emerald-400">exit 0</span>}
            {failed && <span className="text-red-400">failed</span>}
          </div>
        </div>
      )}
    </div>
  );
}
