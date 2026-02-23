"use client";

import { useMemo, useState } from "react";
import type { RunActionEvent } from "@/lib/types";

interface ToolCallInfo {
  tool_name: string;
  arguments: unknown;
  succeeded?: boolean;
  content?: string;
  error?: string;
  duration_ms?: number;
  timestamp: string;
}

interface Props {
  events: RunActionEvent[];
  isRunning: boolean;
}

export function AgentThinking({ events, isRunning }: Props) {
  if (!isRunning) return null;

  // Find currently active nodes (started but not completed/failed)
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

  // Collect streaming tokens per active node
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

  // Collect tool call events
  const toolCalls = useMemo(() => {
    const calls: ToolCallInfo[] = [];
    for (const ev of events) {
      if (ev.action === "mcp_tool_called") {
        const p = ev.payload as Record<string, unknown>;
        calls.push({
          tool_name: (p.tool_name as string) ?? "unknown",
          arguments: p.arguments ?? {},
          succeeded: p.succeeded as boolean | undefined,
          content: p.content as string | undefined,
          error: p.error as string | undefined,
          duration_ms: p.duration_ms as number | undefined,
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
        <div className="space-y-1.5">
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

function ToolCallCard({ call }: { call: ToolCallInfo }) {
  const [open, setOpen] = useState(false);
  const ok = call.succeeded === true;
  const failed = call.succeeded === false;

  return (
    <div
      className={`rounded-lg border p-3 ${
        ok
          ? "border-emerald-200 bg-emerald-50/50"
          : failed
            ? "border-red-200 bg-red-50/50"
            : "border-violet-200 bg-violet-50/50"
      }`}
    >
      {/* Header */}
      <div className="flex items-center gap-2">
        <span className="text-sm">
          {ok ? "\u2705" : failed ? "\u274C" : "\u26A1"}
        </span>
        <span className="font-mono text-xs font-semibold text-slate-700">
          {call.tool_name}
        </span>
        {call.duration_ms != null && (
          <span className="ml-auto text-[10px] text-slate-400">
            {call.duration_ms}ms
          </span>
        )}
      </div>

      {/* Arguments preview */}
      <button
        onClick={() => setOpen(!open)}
        className="mt-1.5 text-[10px] font-medium text-slate-500 hover:text-slate-700"
      >
        {open ? "\u25BC Hide details" : "\u25B6 Show details"}
      </button>

      {open && (
        <div className="mt-2 space-y-2">
          {/* Arguments */}
          <div>
            <div className="text-[10px] font-semibold uppercase tracking-wider text-slate-400">
              Arguments
            </div>
            <pre className="mt-0.5 max-h-32 overflow-y-auto whitespace-pre-wrap rounded bg-white/80 p-2 text-[10px] text-slate-600">
              {JSON.stringify(call.arguments, null, 2)}
            </pre>
          </div>

          {/* Result */}
          {(call.content || call.error) && (
            <div>
              <div className="text-[10px] font-semibold uppercase tracking-wider text-slate-400">
                {failed ? "Error" : "Result"}
              </div>
              <pre className={`mt-0.5 max-h-48 overflow-y-auto whitespace-pre-wrap rounded p-2 text-[10px] ${
                failed
                  ? "bg-red-50 text-red-600"
                  : "bg-white/80 text-slate-600"
              }`}>
                {call.error ?? call.content}
              </pre>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
