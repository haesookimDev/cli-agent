"use client";

import { useState } from "react";
import { apiPost } from "@/lib/api-client";
import { useRunSSE } from "@/hooks/use-sse";
import { StatusBadge } from "@/components/status-badge";
import { RunActions } from "@/components/run-actions";
import { StreamLog } from "@/components/stream-log";
import type { RunSubmission, TaskProfile, RunStatus } from "@/lib/types";

export default function RunnerPage() {
  const [task, setTask] = useState("");
  const [profile, setProfile] = useState<TaskProfile>("general");
  const [sessionId, setSessionId] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [currentRun, setCurrentRun] = useState<RunSubmission | null>(null);
  const { events, terminalStatus, stop } = useRunSSE(
    currentRun?.run_id ?? null,
  );

  const runStatus: RunStatus =
    (terminalStatus as RunStatus) ?? currentRun?.status ?? "queued";

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (!task.trim()) return;
    setError(null);
    setSubmitting(true);

    try {
      const body: Record<string, string> = { task, profile };
      if (sessionId.trim()) body.session_id = sessionId.trim();
      const sub = await apiPost<RunSubmission>("/v1/runs", body);
      setCurrentRun(sub);
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setSubmitting(false);
    }
  }

  function handleReset() {
    stop();
    setCurrentRun(null);
    setTask("");
    setError(null);
  }

  return (
    <div className="space-y-4">
      <form
        onSubmit={handleSubmit}
        className="rounded-xl border border-slate-200 bg-white p-5"
      >
        <h2 className="mb-3 text-sm font-semibold text-slate-700">
          Submit Task
        </h2>

        <textarea
          value={task}
          onChange={(e) => setTask(e.target.value)}
          placeholder="Describe the task for the agent..."
          rows={3}
          className="mb-3 w-full rounded-lg border border-slate-200 bg-slate-50 px-3 py-2 text-sm placeholder:text-slate-400 focus:border-teal-500 focus:outline-none focus:ring-1 focus:ring-teal-500"
        />

        <div className="mb-3 flex gap-3">
          <div className="flex-1">
            <label className="mb-1 block text-xs font-medium text-slate-500">
              Profile
            </label>
            <select
              value={profile}
              onChange={(e) => setProfile(e.target.value as TaskProfile)}
              className="w-full rounded-lg border border-slate-200 bg-slate-50 px-3 py-2 text-sm focus:border-teal-500 focus:outline-none"
            >
              <option value="general">General</option>
              <option value="planning">Planning</option>
              <option value="extraction">Extraction</option>
              <option value="coding">Coding</option>
            </select>
          </div>
          <div className="flex-1">
            <label className="mb-1 block text-xs font-medium text-slate-500">
              Session ID (optional)
            </label>
            <input
              value={sessionId}
              onChange={(e) => setSessionId(e.target.value)}
              placeholder="auto"
              className="w-full rounded-lg border border-slate-200 bg-slate-50 px-3 py-2 text-sm placeholder:text-slate-400 focus:border-teal-500 focus:outline-none"
            />
          </div>
        </div>

        <div className="flex gap-2">
          <button
            type="submit"
            disabled={submitting || !task.trim()}
            className="rounded-lg bg-teal-600 px-4 py-2 text-sm font-medium text-white hover:bg-teal-700 disabled:opacity-50"
          >
            {submitting ? "Submitting..." : "Run Task"}
          </button>
          {currentRun && (
            <button
              type="button"
              onClick={handleReset}
              className="rounded-lg border border-slate-300 px-4 py-2 text-sm font-medium text-slate-600 hover:bg-slate-50"
            >
              New Task
            </button>
          )}
        </div>
      </form>

      {error && (
        <div className="rounded-lg border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700">
          {error}
        </div>
      )}

      {currentRun && (
        <div className="rounded-xl border border-slate-200 bg-white p-5">
          <div className="mb-3 flex items-center justify-between">
            <div className="flex items-center gap-3">
              <h2 className="text-sm font-semibold text-slate-700">
                Run {currentRun.run_id.slice(0, 8)}
              </h2>
              <StatusBadge status={runStatus} />
            </div>
            <RunActions runId={currentRun.run_id} status={runStatus} />
          </div>
          <p className="mb-3 text-xs text-slate-400">
            Session: {currentRun.session_id.slice(0, 8)}
          </p>
          <StreamLog events={events} />
          {terminalStatus && (
            <p className="mt-3 text-xs font-medium text-slate-500">
              Run finished with status:{" "}
              <span className="text-slate-800">{terminalStatus}</span>
            </p>
          )}
        </div>
      )}
    </div>
  );
}
