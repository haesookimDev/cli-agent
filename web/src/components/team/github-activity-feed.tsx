"use client";

import { GitHubActivityItem } from "@/lib/types";

interface GitHubActivityFeedProps {
  activities: GitHubActivityItem[];
}

const activityIcons: Record<string, string> = {
  issue_created: "\u{1F4DD}",
  issue_commented: "\u{1F4AC}",
  issue_closed: "\u2705",
  pr_created: "\u{1F500}",
  pr_reviewed: "\u{1F50D}",
  pr_commented: "\u{1F4AC}",
  pr_merged: "\u{1F7E2}",
  branch_created: "\u{1F33F}",
};

const activityLabels: Record<string, string> = {
  issue_created: "created issue",
  issue_commented: "commented on issue",
  issue_closed: "closed issue",
  pr_created: "opened PR",
  pr_reviewed: "reviewed PR",
  pr_commented: "commented on PR",
  pr_merged: "merged PR",
  branch_created: "created branch",
};

function timeAgo(dateStr: string): string {
  const diff = Date.now() - new Date(dateStr).getTime();
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  return `${Math.floor(hrs / 24)}d ago`;
}

export default function GitHubActivityFeed({
  activities,
}: GitHubActivityFeedProps) {
  if (activities.length === 0) {
    return (
      <div className="text-gray-500 text-sm text-center py-8">
        No GitHub activity yet.
      </div>
    );
  }

  return (
    <div className="space-y-2 max-h-[500px] overflow-y-auto">
      {activities.map((act) => (
        <div
          key={act.id}
          className="flex items-start gap-3 p-3 rounded-lg bg-gray-800/40 hover:bg-gray-800/70 transition-colors"
        >
          <span className="text-lg shrink-0" role="img">
            {activityIcons[act.activity_type] || "\u{1F4CB}"}
          </span>
          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-2">
              <span className="text-white text-sm font-medium">
                {act.persona_name}
              </span>
              <span className="text-gray-500 text-xs">
                {activityLabels[act.activity_type] || act.activity_type}
              </span>
              {act.target_number && (
                <span className="text-blue-400 text-xs">
                  #{act.target_number}
                </span>
              )}
            </div>
            <p className="text-gray-400 text-xs mt-0.5 truncate">
              {act.title}
            </p>
            {act.body_preview && (
              <p className="text-gray-500 text-[11px] mt-1 line-clamp-2">
                {act.body_preview}
              </p>
            )}
            <div className="flex items-center gap-3 mt-1">
              <span className="text-gray-600 text-[10px]">
                {timeAgo(act.created_at)}
              </span>
              {act.github_url && (
                <a
                  href={act.github_url}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="text-blue-500 text-[10px] hover:underline"
                >
                  View on GitHub
                </a>
              )}
            </div>
          </div>
        </div>
      ))}
    </div>
  );
}
