"use client";

import { useState } from "react";

export interface ToolCallInfo {
  tool_name: string;
  arguments: Record<string, unknown>;
  succeeded?: boolean;
  content?: string;
  error?: string;
  duration_ms?: number;
  node_id?: string;
  timestamp: string;
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

export function ToolCallCard({ call }: { call: ToolCallInfo }) {
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

/** Extract tool call info from RunActionEvents */
export function extractToolCalls(events: Array<{ action: string; payload: Record<string, unknown>; actor_id: string | null; timestamp: string }>): ToolCallInfo[] {
  const calls: ToolCallInfo[] = [];
  for (const ev of events) {
    if (ev.action === "mcp_tool_called") {
      const p = ev.payload;
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
}
