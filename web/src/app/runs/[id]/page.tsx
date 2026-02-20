"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { useParams } from "next/navigation";
import { apiGet } from "@/lib/api-client";
import { StatusBadge } from "@/components/status-badge";
import { RunActions } from "@/components/run-actions";
import type { RunRecord } from "@/lib/types";

export default function RunDetailPage() {
  const { id } = useParams<{ id: string }>();
  const [run, setRun] = useState<RunRecord | null>(null);
  const [loading, setLoading] = useState(true);

  function load() {
    if (!id) return;
    setLoading(true);
    apiGet<RunRecord>(`/v1/runs/${id}`)
      .then(setRun)
      .catch((err) => console.error("load run:", err))
      .finally(() => setLoading(false));
  }

  useEffect(() => {
    load();
  }, [id]); // eslint-disable-line react-hooks/exhaustive-deps

  if (loading) {
    return (
      <div className="p-6 text-center text-sm text-slate-400">Loading...</div>
    );
  }

  if (!run) {
    return (
      <div className="p-6 text-center text-sm text-red-500">Run not found</div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-2">
        <Link href="/runs" className="text-xs text-teal-600 hover:underline">
          Runs
        </Link>
        <span className="text-xs text-slate-400">/</span>
        <span className="font-mono text-xs text-slate-600">
          {id?.slice(0, 8)}
        </span>
      </div>

      <div className="rounded-xl border border-slate-200 bg-white p-5">
        <div className="mb-4 flex items-center justify-between">
          <div className="flex items-center gap-3">
            <h2 className="text-sm font-semibold text-slate-700">
              Run Details
            </h2>
            <StatusBadge status={run.status} />
          </div>
          <div className="flex items-center gap-2">
            <Link
              href={`/results?run=${run.run_id}`}
              className="rounded-md border border-slate-300 bg-white px-2 py-1 text-xs font-medium text-slate-700 hover:bg-slate-50"
            >
              Results
            </Link>
            <Link
              href={`/trace?run=${run.run_id}`}
              className="rounded-md border border-slate-300 bg-white px-2 py-1 text-xs font-medium text-slate-700 hover:bg-slate-50"
            >
              Trace
            </Link>
            <RunActions runId={run.run_id} status={run.status} onAction={load} />
          </div>
        </div>

        <dl className="mb-4 grid grid-cols-2 gap-x-6 gap-y-2 text-xs sm:grid-cols-4">
          <div>
            <dt className="text-slate-400">Run ID</dt>
            <dd className="font-mono text-slate-700">{run.run_id.slice(0, 8)}</dd>
          </div>
          <div>
            <dt className="text-slate-400">Session</dt>
            <dd>
              <Link
                href={`/sessions/${run.session_id}`}
                className="font-mono text-teal-600 hover:underline"
              >
                {run.session_id.slice(0, 8)}
              </Link>
            </dd>
          </div>
          <div>
            <dt className="text-slate-400">Profile</dt>
            <dd className="text-slate-700">{run.profile}</dd>
          </div>
          <div>
            <dt className="text-slate-400">Created</dt>
            <dd className="text-slate-700">
              {new Date(run.created_at).toLocaleString()}
            </dd>
          </div>
        </dl>

        <div className="mb-4 rounded-lg bg-slate-50 px-3 py-2 text-sm text-slate-700">
          {run.task}
        </div>

        {run.error && (
          <div className="mb-4 rounded-lg border border-red-200 bg-red-50 px-3 py-2 text-sm text-red-700">
            {run.error}
          </div>
        )}
      </div>

      {run.outputs.length > 0 && (
        <div className="rounded-xl border border-slate-200 bg-white">
          <h3 className="border-b border-slate-100 px-4 py-3 text-xs font-semibold text-slate-700">
            Outputs ({run.outputs.length})
          </h3>
          <div className="overflow-x-auto">
            <table className="w-full text-xs">
              <thead className="border-b border-slate-100 text-left text-slate-500">
                <tr>
                  <th className="px-4 py-2 font-medium">Node</th>
                  <th className="px-4 py-2 font-medium">Role</th>
                  <th className="px-4 py-2 font-medium">Model</th>
                  <th className="px-4 py-2 font-medium">Status</th>
                  <th className="px-4 py-2 font-medium">Duration</th>
                  <th className="px-4 py-2 font-medium">Output</th>
                </tr>
              </thead>
              <tbody className="font-mono">
                {run.outputs.map((o, i) => (
                  <tr key={i} className="border-t border-slate-50">
                    <td className="px-4 py-2 text-slate-700">{o.node_id}</td>
                    <td className="px-4 py-2 text-slate-500">{o.role}</td>
                    <td className="px-4 py-2 text-slate-500">{o.model}</td>
                    <td className="px-4 py-2">
                      <span
                        className={
                          o.succeeded ? "text-emerald-600" : "text-red-600"
                        }
                      >
                        {o.succeeded ? "OK" : "FAILED"}
                      </span>
                    </td>
                    <td className="px-4 py-2 text-slate-500">
                      {o.duration_ms}ms
                    </td>
                    <td className="max-w-xs truncate px-4 py-2 text-slate-600">
                      {o.output || o.error || "-"}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
    </div>
  );
}
