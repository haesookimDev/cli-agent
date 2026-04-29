"use client";

import { useEffect, useState } from "react";
import { apiGet } from "@/lib/api-client";
import { useInterval } from "@/hooks/use-interval";

interface RoleStats {
  active: number;
  completed: number;
  failed: number;
  iterations: number;
  tokens: { input_tokens: number; output_tokens: number };
}

interface HarnessMetricsSnapshot {
  active_sessions: number;
  idle_sessions: number;
  completed_sessions: number;
  failed_sessions: number;
  terminated_sessions: number;
  total_iterations: number;
  total_tokens: { input_tokens: number; output_tokens: number };
  sub_agent_depth: number;
  per_role: Record<string, RoleStats>;
}

const ROLE_ORDER = [
  "planner",
  "extractor",
  "coder",
  "summarizer",
  "fallback",
  "tool_caller",
  "analyzer",
  "reviewer",
  "scheduler",
  "config_manager",
  "validator",
];

export default function HarnessPage() {
  const [data, setData] = useState<HarnessMetricsSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function load() {
    try {
      const m = await apiGet<HarnessMetricsSnapshot>("/v1/harness/metrics");
      setData(m);
      setError(null);
    } catch (e) {
      setError((e as Error).message);
    }
  }

  useEffect(() => {
    load();
  }, []);

  useInterval(load, 2000);

  if (error) {
    return (
      <div className="rounded-lg border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700">
        {error}
      </div>
    );
  }
  if (!data) {
    return (
      <div className="p-6 text-center text-sm text-slate-400">Loading…</div>
    );
  }

  const totalTokens =
    data.total_tokens.input_tokens + data.total_tokens.output_tokens;

  const perRoleEntries = ROLE_ORDER.filter((r) => data.per_role[r]).map(
    (r) => [r, data.per_role[r]] as const,
  );
  const otherRoles = Object.entries(data.per_role).filter(
    ([k]) => !ROLE_ORDER.includes(k),
  );
  const allRows = [...perRoleEntries, ...otherRoles];

  return (
    <div className="space-y-4">
      <h2 className="text-sm font-semibold text-slate-700">
        Harness Metrics
      </h2>

      <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
        {[
          { label: "Active sessions", value: data.active_sessions },
          { label: "Completed", value: data.completed_sessions },
          { label: "Failed", value: data.failed_sessions },
          { label: "Sub-agent depth", value: data.sub_agent_depth },
          { label: "Idle", value: data.idle_sessions },
          { label: "Terminated", value: data.terminated_sessions },
          { label: "Iterations", value: data.total_iterations },
          {
            label: "Tokens (in / out)",
            value: `${data.total_tokens.input_tokens.toLocaleString()} / ${data.total_tokens.output_tokens.toLocaleString()}`,
          },
        ].map((card) => (
          <div
            key={card.label}
            className="rounded-lg border border-slate-200 bg-white px-3 py-2"
          >
            <p className="text-[10px] font-medium uppercase tracking-wider text-slate-400">
              {card.label}
            </p>
            <p className="mt-0.5 text-sm font-semibold text-slate-700">
              {card.value}
            </p>
          </div>
        ))}
      </div>

      <div className="rounded-xl border border-slate-200 bg-white">
        <h3 className="border-b border-slate-100 px-5 py-3 text-xs font-semibold uppercase tracking-wider text-slate-400">
          Per-role activity
        </h3>
        <div className="overflow-x-auto">
          <table className="w-full text-xs">
            <thead className="border-b border-slate-100 text-left text-slate-500">
              <tr>
                <th className="px-4 py-2 font-medium">Role</th>
                <th className="px-4 py-2 font-medium">Active</th>
                <th className="px-4 py-2 font-medium">Completed</th>
                <th className="px-4 py-2 font-medium">Failed</th>
                <th className="px-4 py-2 font-medium">Iterations</th>
                <th className="px-4 py-2 font-medium">Tokens</th>
              </tr>
            </thead>
            <tbody>
              {allRows.length === 0 ? (
                <tr>
                  <td
                    colSpan={6}
                    className="px-4 py-4 text-center text-slate-400"
                  >
                    No activity yet
                  </td>
                </tr>
              ) : (
                allRows.map(([role, s]) => (
                  <tr key={role} className="border-t border-slate-50">
                    <td className="px-4 py-2 font-medium text-slate-700">
                      {role}
                    </td>
                    <td className="px-4 py-2 text-slate-500">{s.active}</td>
                    <td className="px-4 py-2 text-slate-500">{s.completed}</td>
                    <td className="px-4 py-2 text-slate-500">{s.failed}</td>
                    <td className="px-4 py-2 text-slate-500">{s.iterations}</td>
                    <td className="px-4 py-2 font-mono text-[11px] text-slate-500">
                      {(
                        s.tokens.input_tokens + s.tokens.output_tokens
                      ).toLocaleString()}
                    </td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>
      </div>

      <p className="text-[11px] text-slate-400">
        Total tokens across the harness: {totalTokens.toLocaleString()} · Polled
        every 2 seconds.
      </p>
    </div>
  );
}
