"use client";

import { useEffect, useState } from "react";
import { apiGet, apiPost, apiPut } from "@/lib/api-client";
import {
  AgentPersona,
  AgentRole,
  TaskProfile,
  TeamMember,
  TeamMemberInput,
} from "@/lib/types";

interface AgentFormModalProps {
  /** When null, the form opens in "create" mode. */
  member: TeamMember | null;
  onClose: () => void;
  onSaved: () => void;
}

const ROLES: AgentRole[] = [
  "planner",
  "coder",
  "reviewer",
  "validator",
  "analyzer",
  "extractor",
  "summarizer",
  "tool_caller",
  "scheduler",
  "config_manager",
  "fallback",
];

const PROFILES: TaskProfile[] = ["general", "planning", "coding", "extraction"];

const DEFAULT_PERSONA: AgentPersona = {
  display_name: "",
  title: "",
  github_username: "",
  bio: "",
  personality: {
    thoroughness: 0.5,
    creativity: 0.5,
    strictness: 0.5,
    verbosity: 0.5,
  },
  expertise: [],
  communication_style: "balanced",
};

export default function AgentFormModal({
  member,
  onClose,
  onSaved,
}: AgentFormModalProps) {
  const isEdit = member !== null;
  const [name, setName] = useState(member?.name ?? "");
  const [description, setDescription] = useState(member?.description ?? "");
  const [role, setRole] = useState<AgentRole>(member?.role ?? "coder");
  const [taskProfile, setTaskProfile] = useState<TaskProfile>(
    member?.task_profile ?? "general"
  );
  const [capabilities, setCapabilities] = useState(
    (member?.capabilities ?? []).join("\n")
  );
  const [systemPrompt, setSystemPrompt] = useState(member?.system_prompt ?? "");
  const [persona, setPersona] = useState<AgentPersona>(
    member?.persona ?? DEFAULT_PERSONA
  );
  const [expertiseText, setExpertiseText] = useState(
    (member?.persona.expertise ?? []).join(", ")
  );
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // When editing, the list endpoint omits system_prompt; fetch detail.
  useEffect(() => {
    if (!isEdit || !member) return;
    if (member.system_prompt && member.system_prompt.length > 0) return;
    apiGet<TeamMember>(
      `/v1/team/members/${encodeURIComponent(member.name)}`
    )
      .then((detail) => {
        setSystemPrompt(detail.system_prompt ?? "");
      })
      .catch(() => {
        // Leave systemPrompt blank; user must re-enter.
      });
  }, [isEdit, member]);

  async function handleSubmit() {
    if (!name.trim()) {
      setError("Name is required");
      return;
    }
    if (!systemPrompt.trim()) {
      setError("System prompt is required");
      return;
    }

    const payload: TeamMemberInput = {
      name: name.trim(),
      description: description.trim(),
      role,
      task_profile: taskProfile,
      capabilities: capabilities
        .split("\n")
        .map((c) => c.trim())
        .filter(Boolean),
      system_prompt: systemPrompt,
      instructions: "",
      persona: {
        ...persona,
        expertise: expertiseText
          .split(",")
          .map((s) => s.trim())
          .filter(Boolean),
      },
    };

    setSubmitting(true);
    setError(null);
    try {
      if (isEdit && member) {
        await apiPut(
          `/v1/team/members/${encodeURIComponent(member.name)}`,
          payload
        );
      } else {
        await apiPost("/v1/team/members", payload);
      }
      onSaved();
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Save failed");
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
      onClick={onClose}
    >
      <div
        className="bg-white rounded-xl shadow-xl w-full max-w-2xl max-h-[90vh] overflow-y-auto"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="px-6 py-4 border-b border-gray-200 flex items-center justify-between">
          <h3 className="text-lg font-semibold text-slate-800">
            {isEdit ? `Edit ${member?.name}` : "Add Team Member"}
          </h3>
          <button
            onClick={onClose}
            className="text-gray-400 hover:text-gray-600"
            aria-label="Close"
          >
            ✕
          </button>
        </div>

        <div className="px-6 py-4 space-y-4">
          {error && (
            <div className="rounded bg-red-50 border border-red-200 px-3 py-2 text-sm text-red-700">
              {error}
            </div>
          )}

          <div className="grid grid-cols-2 gap-3">
            <Field label="Name (identifier)">
              <input
                type="text"
                value={name}
                onChange={(e) => setName(e.target.value)}
                disabled={isEdit}
                placeholder="Senior Dev Minho"
                className="w-full px-2 py-1 border border-gray-300 rounded text-sm disabled:bg-gray-100"
              />
            </Field>
            <Field label="Role">
              <select
                value={role}
                onChange={(e) => setRole(e.target.value as AgentRole)}
                className="w-full px-2 py-1 border border-gray-300 rounded text-sm"
              >
                {ROLES.map((r) => (
                  <option key={r} value={r}>
                    {r}
                  </option>
                ))}
              </select>
            </Field>
            <Field label="Task profile">
              <select
                value={taskProfile}
                onChange={(e) => setTaskProfile(e.target.value as TaskProfile)}
                className="w-full px-2 py-1 border border-gray-300 rounded text-sm"
              >
                {PROFILES.map((p) => (
                  <option key={p} value={p}>
                    {p}
                  </option>
                ))}
              </select>
            </Field>
            <Field label="Display name">
              <input
                type="text"
                value={persona.display_name}
                onChange={(e) =>
                  setPersona({ ...persona, display_name: e.target.value })
                }
                placeholder="이민호"
                className="w-full px-2 py-1 border border-gray-300 rounded text-sm"
              />
            </Field>
            <Field label="Title">
              <input
                type="text"
                value={persona.title}
                onChange={(e) =>
                  setPersona({ ...persona, title: e.target.value })
                }
                placeholder="Senior Backend Engineer"
                className="w-full px-2 py-1 border border-gray-300 rounded text-sm"
              />
            </Field>
            <Field label="GitHub username">
              <input
                type="text"
                value={persona.github_username}
                onChange={(e) =>
                  setPersona({
                    ...persona,
                    github_username: e.target.value,
                  })
                }
                placeholder="agent-minho-dev"
                className="w-full px-2 py-1 border border-gray-300 rounded text-sm"
              />
            </Field>
          </div>

          <Field label="Description">
            <input
              type="text"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              className="w-full px-2 py-1 border border-gray-300 rounded text-sm"
            />
          </Field>

          <Field label="Bio">
            <textarea
              value={persona.bio}
              onChange={(e) => setPersona({ ...persona, bio: e.target.value })}
              rows={2}
              className="w-full px-2 py-1 border border-gray-300 rounded text-sm"
            />
          </Field>

          <div className="grid grid-cols-2 gap-3">
            <Field label="Expertise (comma-separated)">
              <input
                type="text"
                value={expertiseText}
                onChange={(e) => setExpertiseText(e.target.value)}
                placeholder="rust, backend, performance"
                className="w-full px-2 py-1 border border-gray-300 rounded text-sm"
              />
            </Field>
            <Field label="Communication style">
              <input
                type="text"
                value={persona.communication_style}
                onChange={(e) =>
                  setPersona({
                    ...persona,
                    communication_style: e.target.value,
                  })
                }
                className="w-full px-2 py-1 border border-gray-300 rounded text-sm"
              />
            </Field>
          </div>

          <div className="grid grid-cols-2 gap-3">
            {(
              [
                ["thoroughness", "Thoroughness"],
                ["creativity", "Creativity"],
                ["strictness", "Strictness"],
                ["verbosity", "Verbosity"],
              ] as const
            ).map(([key, label]) => (
              <Field key={key} label={`${label}: ${persona.personality[key].toFixed(2)}`}>
                <input
                  type="range"
                  min={0}
                  max={1}
                  step={0.05}
                  value={persona.personality[key]}
                  onChange={(e) =>
                    setPersona({
                      ...persona,
                      personality: {
                        ...persona.personality,
                        [key]: parseFloat(e.target.value),
                      },
                    })
                  }
                  className="w-full"
                />
              </Field>
            ))}
          </div>

          <Field label="Capabilities (one per line)">
            <textarea
              value={capabilities}
              onChange={(e) => setCapabilities(e.target.value)}
              rows={3}
              className="w-full px-2 py-1 border border-gray-300 rounded text-sm font-mono"
            />
          </Field>

          <Field label="System prompt">
            <textarea
              value={systemPrompt}
              onChange={(e) => setSystemPrompt(e.target.value)}
              rows={6}
              placeholder="You are a senior backend engineer who..."
              className="w-full px-2 py-1 border border-gray-300 rounded text-sm font-mono"
            />
          </Field>
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
            {submitting ? "Saving..." : isEdit ? "Save changes" : "Create"}
          </button>
        </div>
      </div>
    </div>
  );
}

function Field({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <label className="block">
      <span className="text-xs text-slate-600 block mb-1">{label}</span>
      {children}
    </label>
  );
}
