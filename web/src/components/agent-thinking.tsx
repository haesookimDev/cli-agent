"use client";

import type { RunActionEvent } from "@/lib/types";

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

  if (activeNodes.length === 0) {
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

  return (
    <div className="flex items-center gap-2 px-4 py-3">
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
  );
}
