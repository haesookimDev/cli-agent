"use client";

import { use, useCallback, useEffect, useState } from "react";
import Link from "next/link";
import { apiGet, apiPost } from "@/lib/api-client";
import { Meeting, MeetingMessage, TeamMember } from "@/lib/types";

interface PageProps {
  params: Promise<{ id: string; mid: string }>;
}

export default function MeetingDetailPage({ params }: PageProps) {
  const { id: rawId, mid: rawMid } = use(params);
  const workspaceId = decodeURIComponent(rawId);
  const meetingId = decodeURIComponent(rawMid);

  const [meeting, setMeeting] = useState<Meeting | null>(null);
  const [messages, setMessages] = useState<MeetingMessage[]>([]);
  const [members, setMembers] = useState<TeamMember[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [content, setContent] = useState("");
  const [posting, setPosting] = useState(false);

  const reload = useCallback(async () => {
    try {
      const detail = await apiGet<{
        meeting: Meeting;
        messages: MeetingMessage[];
      }>(`/v1/meetings/${encodeURIComponent(meetingId)}`);
      setMeeting(detail.meeting);
      setMessages(detail.messages);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Load failed");
    } finally {
      setLoading(false);
    }
  }, [meetingId]);

  useEffect(() => {
    reload();
    apiGet<TeamMember[]>("/v1/team/members")
      .then(setMembers)
      .catch(() => setMembers([]));
  }, [reload]);

  async function handlePost(e: React.FormEvent) {
    e.preventDefault();
    if (!content.trim() || posting) return;
    setPosting(true);
    try {
      await apiPost(`/v1/meetings/${encodeURIComponent(meetingId)}/messages`, {
        content: content.trim(),
        speaker_kind: "user",
        speaker_name: "user",
      });
      setContent("");
      await reload();
    } catch (e) {
      window.alert(e instanceof Error ? e.message : "Post failed");
    } finally {
      setPosting(false);
    }
  }

  async function handleClose() {
    if (!window.confirm("Close this meeting?")) return;
    try {
      await apiPost(`/v1/meetings/${encodeURIComponent(meetingId)}/close`, {});
      await reload();
    } catch (e) {
      window.alert(e instanceof Error ? e.message : "Close failed");
    }
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64 text-gray-500">
        Loading...
      </div>
    );
  }
  if (error || !meeting) {
    return (
      <div className="space-y-4">
        <Link
          href={`/workspaces/${encodeURIComponent(workspaceId)}`}
          className="text-sm text-blue-600 hover:underline"
        >
          &larr; Back to workspace
        </Link>
        <div className="rounded border border-red-200 bg-red-50 p-4 text-sm text-red-700">
          {error ?? "Meeting not found"}
        </div>
      </div>
    );
  }

  const participantTags = meeting.participants
    .map((p) => members.find((m) => m.name === p))
    .filter(Boolean) as TeamMember[];

  return (
    <div className="space-y-4">
      <Link
        href={`/workspaces/${encodeURIComponent(workspaceId)}`}
        className="text-sm text-blue-600 hover:underline"
      >
        &larr; Back to workspace
      </Link>

      <div className="rounded-xl border border-slate-200 bg-white p-4">
        <div className="flex items-start justify-between">
          <div>
            <h2 className="text-lg font-semibold text-slate-800">
              {meeting.topic}
            </h2>
            <p className="text-xs text-slate-400 mt-0.5">
              {meeting.status} · created {new Date(meeting.created_at).toLocaleString()}
            </p>
          </div>
          {meeting.status === "open" && (
            <button
              onClick={handleClose}
              className="px-3 py-1 text-xs border border-slate-300 rounded hover:bg-slate-50"
            >
              Close meeting
            </button>
          )}
        </div>
        {participantTags.length > 0 && (
          <div className="flex flex-wrap gap-1 mt-3">
            {participantTags.map((m) => (
              <span
                key={m.name}
                className="px-2 py-0.5 text-[11px] rounded bg-slate-100 text-slate-700"
              >
                {m.persona.display_name} · {m.role}
              </span>
            ))}
          </div>
        )}
      </div>

      <div className="rounded-xl border border-slate-200 bg-white p-4">
        <h3 className="text-sm font-semibold text-slate-700 mb-3">
          Transcript ({messages.length})
        </h3>
        {messages.length === 0 ? (
          <p className="text-sm text-slate-500">No messages yet.</p>
        ) : (
          <ul className="space-y-3">
            {messages.map((m) => (
              <li
                key={m.id}
                className="rounded-lg border border-slate-100 bg-slate-50 p-3"
              >
                <div className="flex items-center justify-between mb-1">
                  <span className="text-xs font-medium text-slate-700">
                    {m.speaker_name}
                    <span className="text-slate-400 ml-1">· {m.speaker_kind}</span>
                  </span>
                  <span className="text-[10px] text-slate-400">
                    {new Date(m.created_at).toLocaleString()}
                  </span>
                </div>
                <p className="text-sm text-slate-700 whitespace-pre-wrap">
                  {m.content}
                </p>
              </li>
            ))}
          </ul>
        )}
      </div>

      {meeting.status === "open" && (
        <form
          onSubmit={handlePost}
          className="rounded-xl border border-slate-200 bg-white p-4 space-y-2"
        >
          <textarea
            value={content}
            onChange={(e) => setContent(e.target.value)}
            placeholder="Add to the discussion..."
            rows={3}
            className="w-full px-2 py-1 border border-slate-300 rounded text-sm"
          />
          <div className="flex justify-end">
            <button
              type="submit"
              disabled={posting || !content.trim()}
              className="px-3 py-1.5 text-sm bg-blue-600 text-white rounded hover:bg-blue-700 disabled:bg-blue-300"
            >
              {posting ? "Posting..." : "Post message"}
            </button>
          </div>
        </form>
      )}
    </div>
  );
}
