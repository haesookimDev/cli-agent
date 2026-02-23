"use client";

import { useMemo, useState } from "react";
import type { RunActionEvent } from "@/lib/types";

const actionColors: Record<string, string> = {
  // Orchestrator decisions
  model_selected: "text-amber-600",
  dynamic_node_added: "text-amber-600",
  // Graph lifecycle
  graph_initialized: "text-blue-600",
  graph_completed: "text-blue-600",
  // Node events
  node_started: "text-sky-600",
  node_completed: "text-emerald-600",
  node_failed: "text-red-600",
  node_skipped: "text-slate-400",
  node_token_chunk: "text-cyan-600",
  // Run lifecycle
  run_queued: "text-teal-600",
  run_started: "text-teal-600",
  run_finished: "text-teal-600",
  run_cancel_requested: "text-orange-600",
  run_pause_requested: "text-orange-600",
  run_resumed: "text-teal-600",
  // Webhook
  webhook_dispatched: "text-purple-600",
  // MCP & Subtask
  mcp_tool_called: "text-violet-600",
  subtask_planned: "text-amber-600",
};

type DisplayRow = {
  kind: "event";
  ev: RunActionEvent;
} | {
  kind: "token_merged";
  nodeId: string;
  text: string;
  count: number;
  firstSeq: number;
  firstTimestamp: string;
  lastTimestamp: string;
};

/** Merge consecutive node_token_chunk events per node into single rows. */
function mergeTokenChunks(events: RunActionEvent[]): DisplayRow[] {
  const rows: DisplayRow[] = [];
  // Accumulate token chunks per node_id
  const pending = new Map<string, { text: string; count: number; firstSeq: number; firstTimestamp: string; lastTimestamp: string }>();

  function flushPending() {
    for (const [nodeId, acc] of pending) {
      rows.push({ kind: "token_merged", nodeId, ...acc });
    }
    pending.clear();
  }

  for (const ev of events) {
    if (ev.action === "node_token_chunk") {
      const p = ev.payload as Record<string, unknown>;
      const nodeId = (p.node_id as string) ?? ev.actor_id ?? "unknown";
      const token = (p.token as string) ?? "";
      const existing = pending.get(nodeId);
      if (existing) {
        existing.text += token;
        existing.count += 1;
        existing.lastTimestamp = ev.timestamp;
      } else {
        pending.set(nodeId, { text: token, count: 1, firstSeq: ev.seq, firstTimestamp: ev.timestamp, lastTimestamp: ev.timestamp });
      }
    } else {
      // Flush any accumulated tokens before a non-token event
      flushPending();
      rows.push({ kind: "event", ev });
    }
  }
  flushPending();
  return rows;
}

interface Props {
  events: RunActionEvent[];
}

export function EventTimeline({ events }: Props) {
  const [expanded, setExpanded] = useState<Set<number>>(new Set());
  const rows = useMemo(() => mergeTokenChunks(events), [events]);

  if (events.length === 0) {
    return (
      <div className="rounded-lg border border-dashed border-slate-300 p-6 text-center text-sm text-slate-400">
        No events recorded
      </div>
    );
  }

  function toggle(seq: number) {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(seq)) next.delete(seq);
      else next.add(seq);
      return next;
    });
  }

  return (
    <div className="max-h-[32rem] overflow-y-auto rounded-lg border border-slate-200 bg-white">
      <table className="w-full text-xs">
        <thead className="sticky top-0 bg-slate-50 text-left text-slate-500">
          <tr>
            <th className="px-3 py-2 font-medium">#</th>
            <th className="px-3 py-2 font-medium">Action</th>
            <th className="px-3 py-2 font-medium">Actor</th>
            <th className="px-3 py-2 font-medium">Time</th>
          </tr>
        </thead>
        <tbody className="font-mono">
          {rows.map((row, i) => {
            if (row.kind === "token_merged") {
              const isExpanded = expanded.has(row.firstSeq);
              return (
                <tr
                  key={`tok-${row.nodeId}-${row.firstSeq}`}
                  className="cursor-pointer border-t border-slate-100 hover:bg-slate-50"
                  onClick={() => toggle(row.firstSeq)}
                >
                  <td className="px-3 py-1.5 text-slate-400">{row.firstSeq}</td>
                  <td className="px-3 py-1.5 font-medium text-cyan-600">
                    node_token_chunk
                    <span className="ml-1.5 rounded bg-cyan-50 px-1 py-0.5 text-[10px] text-cyan-500">
                      {row.count} chunks
                    </span>
                  </td>
                  <td className="px-3 py-1.5 text-slate-500">{row.nodeId}</td>
                  <td className="px-3 py-1.5 text-slate-400">
                    <div>{new Date(row.firstTimestamp).toLocaleTimeString()}</div>
                    {isExpanded && (
                      <pre className="mt-2 max-h-48 overflow-y-auto whitespace-pre-wrap rounded bg-slate-100 p-2 text-[10px] text-slate-600">
                        {row.text}
                      </pre>
                    )}
                  </td>
                </tr>
              );
            }

            const ev = row.ev;
            const isExpanded = expanded.has(ev.seq);
            const hasPayload =
              ev.payload && Object.keys(ev.payload).length > 0;
            const colorClass = actionColors[ev.action] ?? "text-slate-800";
            return (
              <tr
                key={ev.seq}
                className={`border-t border-slate-100 ${hasPayload ? "cursor-pointer hover:bg-slate-50" : ""}`}
                onClick={() => hasPayload && toggle(ev.seq)}
              >
                <td className="px-3 py-1.5 text-slate-400">{ev.seq}</td>
                <td className={`px-3 py-1.5 font-medium ${colorClass}`}>
                  {ev.action}
                </td>
                <td className="px-3 py-1.5 text-slate-500">
                  {ev.actor_id ?? "-"}
                </td>
                <td className="px-3 py-1.5 text-slate-400">
                  <div>
                    {new Date(ev.timestamp).toLocaleTimeString()}
                  </div>
                  {isExpanded && hasPayload && (
                    <pre className="mt-2 max-h-48 overflow-y-auto whitespace-pre-wrap rounded bg-slate-100 p-2 text-[10px] text-slate-600">
                      {JSON.stringify(ev.payload, null, 2)}
                    </pre>
                  )}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
