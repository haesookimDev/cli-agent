"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { apiGet } from "@/lib/api-client";
import { StatusBadge } from "@/components/status-badge";
import { RunActions } from "@/components/run-actions";
import type { RunRecord } from "@/lib/types";

export default function RunsPage() {
  const [runs, setRuns] = useState<RunRecord[]>([]);
  const [loading, setLoading] = useState(true);

  async function load() {
    setLoading(true);
    try {
      const data = await apiGet<RunRecord[]>("/v1/runs?limit=50");
      setRuns(data);
    } catch (err) {
      console.error("load runs:", err);
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    load();
  }, []);

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h2 className="text-sm font-semibold text-slate-700">Recent Runs</h2>
        <button
          onClick={load}
          className="rounded-lg border border-slate-300 px-3 py-1.5 text-xs font-medium text-slate-600 hover:bg-slate-50"
        >
          Refresh
        </button>
      </div>

      <div className="rounded-xl border border-slate-200 bg-white">
        {loading ? (
          <div className="p-6 text-center text-sm text-slate-400">
            Loading...
          </div>
        ) : runs.length === 0 ? (
          <div className="p-6 text-center text-sm text-slate-400">
            No runs yet
          </div>
        ) : (
          <table className="w-full text-sm">
            <thead className="border-b border-slate-100 text-left text-xs text-slate-500">
              <tr>
                <th className="px-4 py-3 font-medium">Run ID</th>
                <th className="px-4 py-3 font-medium">Status</th>
                <th className="px-4 py-3 font-medium">Profile</th>
                <th className="px-4 py-3 font-medium">Task</th>
                <th className="px-4 py-3 font-medium">Created</th>
                <th className="px-4 py-3 font-medium">Actions</th>
              </tr>
            </thead>
            <tbody>
              {runs.map((r) => (
                <tr
                  key={r.run_id}
                  className="border-t border-slate-50 hover:bg-slate-50"
                >
                  <td className="px-4 py-3">
                    <Link
                      href={`/runs/${r.run_id}`}
                      className="font-mono text-xs text-teal-600 hover:underline"
                    >
                      {r.run_id.slice(0, 8)}
                    </Link>
                  </td>
                  <td className="px-4 py-3">
                    <StatusBadge status={r.status} />
                  </td>
                  <td className="px-4 py-3 text-xs text-slate-500">
                    {r.profile}
                  </td>
                  <td className="max-w-xs truncate px-4 py-3 text-xs text-slate-600">
                    {r.task}
                  </td>
                  <td className="px-4 py-3 text-xs text-slate-400">
                    {new Date(r.created_at).toLocaleString()}
                  </td>
                  <td className="px-4 py-3">
                    <div className="flex gap-1">
                      <RunActions
                        runId={r.run_id}
                        status={r.status}
                        onAction={load}
                      />
                      <Link
                        href={`/behavior?run=${r.run_id}`}
                        className="rounded-md border border-slate-300 bg-white px-2 py-1 text-xs font-medium text-slate-700 hover:bg-slate-50"
                      >
                        Behavior
                      </Link>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}
