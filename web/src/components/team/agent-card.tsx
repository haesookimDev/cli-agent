"use client";

import Link from "next/link";
import { TeamMember } from "@/lib/types";

interface AgentCardProps {
  member: TeamMember;
  status?: "idle" | "working" | "reviewing" | "waiting";
  currentTask?: string;
  stats?: {
    issuesCreated: number;
    prsCreated: number;
    reviewsDone: number;
  };
  onEdit?: (member: TeamMember) => void;
  onDelete?: (member: TeamMember) => void;
  onAssign?: (member: TeamMember) => void;
}

const statusColors: Record<string, string> = {
  idle: "bg-gray-400",
  working: "bg-blue-500 animate-pulse",
  reviewing: "bg-yellow-500 animate-pulse",
  waiting: "bg-orange-400",
};

const statusLabels: Record<string, string> = {
  idle: "Idle",
  working: "Working",
  reviewing: "Reviewing",
  waiting: "Waiting",
};

export default function AgentCard({
  member,
  status = "idle",
  currentTask,
  stats,
  onEdit,
  onDelete,
  onAssign,
}: AgentCardProps) {
  const persona = member.persona;

  return (
    <div className="rounded-xl border border-gray-700 bg-gray-800/60 p-4 hover:border-gray-500 transition-colors">
      {/* Header */}
      <div className="flex items-start gap-3">
        <div className="w-12 h-12 rounded-full bg-gradient-to-br from-blue-500 to-purple-600 flex items-center justify-center text-white text-lg font-bold shrink-0">
          {persona.display_name.charAt(0)}
        </div>
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <h3 className="text-white font-semibold truncate">
              {persona.display_name}
            </h3>
            <span
              className={`w-2.5 h-2.5 rounded-full ${statusColors[status]}`}
              title={statusLabels[status]}
            />
          </div>
          <p className="text-gray-400 text-sm">{persona.title}</p>
          <p className="text-gray-500 text-xs mt-0.5">
            @{persona.github_username}
          </p>
        </div>
      </div>

      {/* Bio */}
      <p className="text-gray-400 text-xs mt-3 line-clamp-2">{persona.bio}</p>

      {/* Expertise */}
      <div className="flex flex-wrap gap-1 mt-3">
        {persona.expertise.slice(0, 4).map((tag) => (
          <span
            key={tag}
            className="px-1.5 py-0.5 text-[10px] rounded bg-gray-700 text-gray-300"
          >
            {tag}
          </span>
        ))}
      </div>

      {/* Personality Bars */}
      <div className="mt-3 space-y-1">
        {(
          [
            ["Thoroughness", persona.personality.thoroughness],
            ["Strictness", persona.personality.strictness],
            ["Creativity", persona.personality.creativity],
          ] as [string, number][]
        ).map(([label, value]) => (
          <div key={label} className="flex items-center gap-2 text-[10px]">
            <span className="text-gray-500 w-20">{label}</span>
            <div className="flex-1 h-1.5 bg-gray-700 rounded-full overflow-hidden">
              <div
                className="h-full bg-blue-500 rounded-full"
                style={{ width: `${value * 100}%` }}
              />
            </div>
          </div>
        ))}
      </div>

      {/* Current Task */}
      {currentTask && (
        <div className="mt-3 p-2 rounded bg-gray-900 text-xs text-gray-300 truncate">
          {currentTask}
        </div>
      )}

      {/* Stats */}
      {stats && (
        <div className="flex gap-3 mt-3 text-[10px] text-gray-500">
          <span>Issues: {stats.issuesCreated}</span>
          <span>PRs: {stats.prsCreated}</span>
          <span>Reviews: {stats.reviewsDone}</span>
        </div>
      )}

      {(onEdit || onDelete || onAssign) && (
        <div className="flex flex-wrap gap-2 mt-3 pt-3 border-t border-gray-700">
          {onAssign && (
            <button
              onClick={() => onAssign(member)}
              className="flex-1 min-w-[80px] px-2 py-1 text-xs rounded bg-blue-600 text-white hover:bg-blue-500"
            >
              Assign task
            </button>
          )}
          <Link
            href={`/team/${encodeURIComponent(member.name)}`}
            className="px-2 py-1 text-xs rounded bg-gray-700 text-gray-200 hover:bg-gray-600"
          >
            View runs
          </Link>
          {onEdit && (
            <button
              onClick={() => onEdit(member)}
              className="px-2 py-1 text-xs rounded bg-gray-700 text-gray-200 hover:bg-gray-600"
            >
              Edit
            </button>
          )}
          {onDelete && (
            <button
              onClick={() => onDelete(member)}
              className="px-2 py-1 text-xs rounded bg-gray-700 text-red-300 hover:bg-red-900"
            >
              Delete
            </button>
          )}
        </div>
      )}
    </div>
  );
}
