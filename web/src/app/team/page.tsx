"use client";

import { useEffect, useState } from "react";
import { apiGet } from "@/lib/api-client";
import { TeamMember, GitHubActivityItem } from "@/lib/types";
import AgentCard from "@/components/team/agent-card";
import GitHubActivityFeed from "@/components/team/github-activity-feed";
import InteractionGraph from "@/components/team/interaction-graph";

export default function TeamPage() {
  const [members, setMembers] = useState<TeamMember[]>([]);
  const [activities, setActivities] = useState<GitHubActivityItem[]>([]);
  const [stats, setStats] = useState<
    { persona_name: string; activity_type: string; count: number }[]
  >([]);
  const [loading, setLoading] = useState(true);
  const [activeTab, setActiveTab] = useState<"members" | "activity" | "graph">(
    "members"
  );

  useEffect(() => {
    async function load() {
      try {
        const [m, a, s] = await Promise.all([
          apiGet<TeamMember[]>("/v1/team/members").catch(() => []),
          apiGet<GitHubActivityItem[]>("/v1/github/activities?limit=50").catch(
            () => []
          ),
          apiGet<
            { persona_name: string; activity_type: string; count: number }[]
          >("/v1/github/activities/stats").catch(() => []),
        ]);
        setMembers(m);
        setActivities(a);
        setStats(s);
      } finally {
        setLoading(false);
      }
    }
    load();
  }, []);

  function getStatsForMember(name: string) {
    const memberStats = stats.filter((s) => s.persona_name === name);
    return {
      issuesCreated:
        memberStats.find((s) => s.activity_type === "issue_created")?.count ??
        0,
      prsCreated:
        memberStats.find((s) => s.activity_type === "pr_created")?.count ?? 0,
      reviewsDone:
        memberStats.find((s) => s.activity_type === "pr_reviewed")?.count ?? 0,
    };
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64 text-gray-500">
        Loading team...
      </div>
    );
  }

  const personaNames = members.map((m) => m.persona.display_name);

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-2xl font-bold text-slate-800">
            Virtual Dev Team
          </h2>
          <p className="text-sm text-slate-500 mt-1">
            {members.length} team members &middot; {activities.length} GitHub
            activities
          </p>
        </div>
      </div>

      {/* Tabs */}
      <div className="flex gap-1 p-1 rounded-lg bg-gray-100 w-fit">
        {(["members", "activity", "graph"] as const).map((tab) => (
          <button
            key={tab}
            onClick={() => setActiveTab(tab)}
            className={`px-4 py-2 text-sm rounded-md transition-colors ${
              activeTab === tab
                ? "bg-white text-slate-900 shadow-sm"
                : "text-slate-500 hover:text-slate-700"
            }`}
          >
            {tab === "members"
              ? "Team Members"
              : tab === "activity"
                ? "GitHub Activity"
                : "Interactions"}
          </button>
        ))}
      </div>

      {/* Tab Content */}
      {activeTab === "members" && (
        <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-4">
          {members.map((member) => (
            <AgentCard
              key={member.name}
              member={member}
              stats={getStatsForMember(member.persona.display_name)}
            />
          ))}
          {members.length === 0 && (
            <div className="col-span-3 text-center py-12 text-gray-500">
              No team members configured. Add agent YAML files to{" "}
              <code className="bg-gray-100 px-1 rounded">agents/team/</code>{" "}
              directory.
            </div>
          )}
        </div>
      )}

      {activeTab === "activity" && (
        <div className="rounded-xl border border-gray-200 bg-white p-4">
          <GitHubActivityFeed activities={activities} />
        </div>
      )}

      {activeTab === "graph" && (
        <div className="rounded-xl border border-gray-200 bg-white p-6">
          <InteractionGraph
            activities={activities}
            personas={personaNames}
          />
        </div>
      )}
    </div>
  );
}
