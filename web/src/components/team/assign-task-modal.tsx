"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { apiPost } from "@/lib/api-client";
import { RunSubmission, TeamMember } from "@/lib/types";

interface AssignTaskModalProps {
  member: TeamMember;
  onClose: () => void;
}

export default function AssignTaskModal({
  member,
  onClose,
}: AssignTaskModalProps) {
  const router = useRouter();
  const [task, setTask] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function handleSubmit() {
    if (!task.trim()) {
      setError("Task description is required");
      return;
    }
    setSubmitting(true);
    setError(null);
    try {
      const sub = await apiPost<RunSubmission>("/v1/runs", {
        task: task.trim(),
        assignee: member.name,
      });
      router.push(`/runs/${sub.run_id}`);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to submit run");
      setSubmitting(false);
    }
  }

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
      onClick={onClose}
    >
      <div
        className="bg-white rounded-xl shadow-xl w-full max-w-lg"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="px-6 py-4 border-b border-gray-200 flex items-center justify-between">
          <div>
            <h3 className="text-lg font-semibold text-slate-800">
              Assign task to {member.persona.display_name}
            </h3>
            <p className="text-xs text-slate-500 mt-0.5">
              {member.persona.title} &middot; role: {member.role}
            </p>
          </div>
          <button
            onClick={onClose}
            className="text-gray-400 hover:text-gray-600"
            aria-label="Close"
          >
            ✕
          </button>
        </div>

        <div className="px-6 py-4 space-y-3">
          {error && (
            <div className="rounded bg-red-50 border border-red-200 px-3 py-2 text-sm text-red-700">
              {error}
            </div>
          )}
          <label className="block">
            <span className="text-xs text-slate-600 block mb-1">
              Task description
            </span>
            <textarea
              value={task}
              onChange={(e) => setTask(e.target.value)}
              rows={5}
              autoFocus
              placeholder={`What should ${member.persona.display_name} do?`}
              className="w-full px-2 py-1 border border-gray-300 rounded text-sm"
            />
          </label>
          <p className="text-xs text-slate-500">
            Same-role nodes in the resulting run will be pinned to this
            persona. Other-role nodes auto-route through the team.
          </p>
        </div>

        <div className="px-6 py-4 border-t border-gray-200 flex justify-end gap-2">
          <button
            onClick={onClose}
            className="px-3 py-1.5 text-sm text-slate-600 hover:bg-gray-100 rounded"
          >
            Cancel
          </button>
          <button
            onClick={handleSubmit}
            disabled={submitting}
            className="px-3 py-1.5 text-sm bg-blue-600 text-white rounded hover:bg-blue-700 disabled:bg-blue-300"
          >
            {submitting ? "Submitting..." : "Assign & run"}
          </button>
        </div>
      </div>
    </div>
  );
}
