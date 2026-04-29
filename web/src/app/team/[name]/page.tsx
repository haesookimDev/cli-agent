"use client";

import { use, useEffect, useState } from "react";
import Link from "next/link";
import { apiGet } from "@/lib/api-client";
import { RunRecord, TeamMember } from "@/lib/types";

interface PageProps {
  params: Promise<{ name: string }>;
}

export default function TeamMemberPage({ params }: PageProps) {
  const { name: rawName } = use(params);
  const name = decodeURIComponent(rawName);

  const [member, setMember] = useState<TeamMember | null>(null);
  const [runs, setRuns] = useState<RunRecord[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    async function load() {
      try {
        const [m, r] = await Promise.all([
          apiGet<TeamMember>(
            `/v1/team/members/${encodeURIComponent(name)}`
          ),
          apiGet<RunRecord[]>(
            `/v1/team/members/${encodeURIComponent(name)}/runs?limit=50`
          ).catch(() => []),
        ]);
        if (cancelled) return;
        setMember(m);
        setRuns(r);
      } catch (e) {
        if (cancelled) return;
        setError(e instanceof Error ? e.message : "Failed to load member");
      } finally {
        if (!cancelled) setLoading(false);
      }
    }
    load();
    return () => {
      cancelled = true;
    };
  }, [name]);

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64 text-gray-500">
        Loading {name}...
      </div>
    );
  }

  if (error || !member) {
    return (
      <div className="space-y-4">
        <Link
          href="/team"
          className="text-sm text-blue-600 hover:underline"
        >
          &larr; Back to team
        </Link>
        <div className="rounded border border-red-200 bg-red-50 p-4 text-sm text-red-700">
          {error ?? "Member not found"}
        </div>
      </div>
    );
  }

  const persona = member.persona;

  return (
    <div className="space-y-6">
      <Link
        href="/team"
        className="text-sm text-blue-600 hover:underline"
      >
        &larr; Back to team
      </Link>

      <div className="flex items-start gap-4">
        <div className="w-16 h-16 rounded-full bg-gradient-to-br from-blue-500 to-purple-600 flex items-center justify-center text-white text-2xl font-bold shrink-0">
          {persona.display_name.charAt(0)}
        </div>
        <div className="flex-1 min-w-0">
          <h2 className="text-2xl font-bold text-slate-800">
            {persona.display_name}
          </h2>
          <p className="text-slate-500">{persona.title}</p>
          <p className="text-xs text-slate-400 mt-0.5">
            @{persona.github_username} · role: {member.role} · profile:{" "}
            {member.task_profile}
          </p>
          <p className="text-sm text-slate-600 mt-2">{persona.bio}</p>
          <div className="flex flex-wrap gap-1 mt-2">
            {persona.expertise.map((tag) => (
              <span
                key={tag}
                className="px-1.5 py-0.5 text-[10px] rounded bg-slate-200 text-slate-700"
              >
                {tag}
              </span>
            ))}
          </div>
        </div>
        <Link
          href={`/team/chat?assignee=${encodeURIComponent(member.name)}`}
          className="px-3 py-1.5 text-sm bg-blue-600 text-white rounded hover:bg-blue-700"
        >
          Start chat
        </Link>
      </div>

      <div>
        <h3 className="text-sm font-semibold text-slate-700 mb-2">
          Recent runs ({runs.length})
        </h3>
        {runs.length === 0 ? (
          <div className="rounded border border-slate-200 bg-slate-50 p-4 text-sm text-slate-500">
            No runs assigned to this member yet. Submit a task via{" "}
            <Link
              href={`/team/chat?assignee=${encodeURIComponent(member.name)}`}
              className="text-blue-600 hover:underline"
            >
              Team Chat
            </Link>
            {" "}or the &quot;Assign task&quot; button on the team page.
          </div>
        ) : (
          <ul className="divide-y divide-slate-200 rounded border border-slate-200 bg-white">
            {runs.map((r) => (
              <li key={r.run_id}>
                <Link
                  href={`/runs/${r.run_id}`}
                  className="block px-3 py-2 hover:bg-slate-50"
                >
                  <div className="flex items-center justify-between gap-3">
                    <div className="flex-1 min-w-0">
                      <p className="text-sm text-slate-800 truncate">
                        {r.task}
                      </p>
                      <p className="text-xs text-slate-400 mt-0.5">
                        {new Date(r.created_at).toLocaleString()} · run{" "}
                        {r.run_id.slice(0, 8)}
                      </p>
                    </div>
                    <StatusPill status={r.status} />
                  </div>
                </Link>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

function StatusPill({ status }: { status: string }) {
  const color =
    status === "succeeded"
      ? "bg-emerald-100 text-emerald-700"
      : status === "failed"
        ? "bg-red-100 text-red-700"
        : status === "cancelled"
          ? "bg-slate-200 text-slate-600"
          : "bg-amber-100 text-amber-700";
  return (
    <span className={`px-2 py-0.5 text-[10px] rounded font-medium ${color}`}>
      {status}
    </span>
  );
}
