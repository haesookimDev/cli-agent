"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { apiGet, apiPost } from "@/lib/api-client";
import type { SessionSummary } from "@/lib/types";

export default function SessionsPage() {
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [loading, setLoading] = useState(true);

  async function load() {
    setLoading(true);
    try {
      const data = await apiGet<SessionSummary[]>("/v1/sessions?limit=100");
      setSessions(data);
    } catch (err) {
      console.error("load sessions:", err);
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    load();
  }, []);

  async function createSession() {
    try {
      await apiPost("/v1/sessions", {});
      load();
    } catch (err) {
      console.error("create session:", err);
    }
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h2 className="text-sm font-semibold text-slate-700">Sessions</h2>
        <div className="flex gap-2">
          <button
            onClick={load}
            className="rounded-lg border border-slate-300 px-3 py-1.5 text-xs font-medium text-slate-600 hover:bg-slate-50"
          >
            Refresh
          </button>
          <button
            onClick={createSession}
            className="rounded-lg bg-teal-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-teal-700"
          >
            New Session
          </button>
        </div>
      </div>

      <div className="rounded-xl border border-slate-200 bg-white">
        {loading ? (
          <div className="p-6 text-center text-sm text-slate-400">
            Loading...
          </div>
        ) : sessions.length === 0 ? (
          <div className="p-6 text-center text-sm text-slate-400">
            No sessions yet
          </div>
        ) : (
          <table className="w-full text-sm">
            <thead className="border-b border-slate-100 text-left text-xs text-slate-500">
              <tr>
                <th className="px-4 py-3 font-medium">Session ID</th>
                <th className="px-4 py-3 font-medium">Runs</th>
                <th className="px-4 py-3 font-medium">Last Task</th>
                <th className="px-4 py-3 font-medium">Last Run</th>
                <th className="px-4 py-3 font-medium">Created</th>
              </tr>
            </thead>
            <tbody>
              {sessions.map((s) => (
                <tr
                  key={s.session_id}
                  className="border-t border-slate-50 hover:bg-slate-50"
                >
                  <td className="px-4 py-3">
                    <Link
                      href={`/sessions/${s.session_id}`}
                      className="font-mono text-xs text-teal-600 hover:underline"
                    >
                      {s.session_id.slice(0, 8)}
                    </Link>
                  </td>
                  <td className="px-4 py-3 text-slate-600">{s.run_count}</td>
                  <td className="max-w-xs truncate px-4 py-3 text-xs text-slate-500">
                    {s.last_task ?? "-"}
                  </td>
                  <td className="px-4 py-3 text-xs text-slate-400">
                    {s.last_run_at
                      ? new Date(s.last_run_at).toLocaleString()
                      : "-"}
                  </td>
                  <td className="px-4 py-3 text-xs text-slate-400">
                    {new Date(s.created_at).toLocaleString()}
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
