"use client";

import { apiPost } from "@/lib/api-client";

interface Props {
  runId: string;
  status: string;
  onAction?: () => void;
}

const actions = [
  { key: "cancel", label: "Cancel", when: ["queued", "running"] },
  { key: "pause", label: "Pause", when: ["running"] },
  { key: "resume", label: "Resume", when: ["paused"] },
  { key: "retry", label: "Retry", when: ["failed", "cancelled"] },
] as const;

export function RunActions({ runId, status, onAction }: Props) {
  const visible = actions.filter((a) => a.when.includes(status as never));
  if (visible.length === 0) return null;

  async function exec(action: string) {
    try {
      await apiPost(`/v1/runs/${runId}/${action}`);
      onAction?.();
    } catch (err) {
      console.error(`${action} failed:`, err);
    }
  }

  return (
    <div className="flex gap-1">
      {visible.map((a) => (
        <button
          key={a.key}
          onClick={() => exec(a.key)}
          className="rounded-md border border-slate-300 bg-white px-2 py-1 text-xs font-medium text-slate-700 hover:bg-slate-50"
        >
          {a.label}
        </button>
      ))}
    </div>
  );
}
