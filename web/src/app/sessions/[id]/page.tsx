"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { useParams } from "next/navigation";
import { apiGet } from "@/lib/api-client";
import { StatusBadge } from "@/components/status-badge";
import type { RunRecord } from "@/lib/types";

export default function SessionDetailPage() {
  const { id } = useParams<{ id: string }>();
  const [runs, setRuns] = useState<RunRecord[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    if (!id) return;
    setLoading(true);
    apiGet<RunRecord[]>(`/v1/sessions/${id}/runs?limit=50`)
      .then(setRuns)
      .catch((err) => console.error("load session runs:", err))
      .finally(() => setLoading(false));
  }, [id]);

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-2">
        <Link
          href="/sessions"
          className="text-xs text-teal-600 hover:underline"
        >
          Sessions
        </Link>
        <span className="text-xs text-slate-400">/</span>
        <span className="font-mono text-xs text-slate-600">
          {id?.slice(0, 8)}
        </span>
      </div>

      <div className="rounded-xl border border-slate-200 bg-white">
        {loading ? (
          <div className="p-6 text-center text-sm text-slate-400">
            Loading...
          </div>
        ) : runs.length === 0 ? (
          <div className="p-6 text-center text-sm text-slate-400">
            No runs in this session
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
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}
