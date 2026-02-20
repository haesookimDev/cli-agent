"use client";

import { useEffect, useRef, useState, useCallback } from "react";
import { apiGet, apiPost } from "@/lib/api-client";
import { useRunSSE } from "@/hooks/use-sse";
import { ChatBubble } from "@/components/chat-bubble";
import { AgentThinking } from "@/components/agent-thinking";
import type {
  ChatMessage,
  SessionSummary,
  RunSubmission,
  RunRecord,
  TaskProfile,
  RunStatus,
} from "@/lib/types";

export default function ChatPage() {
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [task, setTask] = useState("");
  const [profile, setProfile] = useState<TaskProfile>("general");
  const [submitting, setSubmitting] = useState(false);

  const [currentRun, setCurrentRun] = useState<RunSubmission | null>(null);
  const { events, terminalStatus, sseError } = useRunSSE(
    currentRun?.run_id ?? null,
  );

  const scrollRef = useRef<HTMLDivElement>(null);

  // Load sessions
  useEffect(() => {
    apiGet<SessionSummary[]>("/v1/sessions?limit=50")
      .then(setSessions)
      .catch(() => {});
  }, []);

  // Load messages when session changes
  const loadMessages = useCallback(async (sid: string) => {
    try {
      const msgs = await apiGet<ChatMessage[]>(
        `/v1/sessions/${sid}/messages?limit=200`,
      );
      setMessages(msgs);
    } catch {
      setMessages([]);
    }
  }, []);

  useEffect(() => {
    if (activeSessionId) {
      loadMessages(activeSessionId);
    } else {
      setMessages([]);
    }
  }, [activeSessionId, loadMessages]);

  // Reload messages when run finishes
  useEffect(() => {
    if (terminalStatus && activeSessionId) {
      loadMessages(activeSessionId);
      setCurrentRun(null);
    }
  }, [terminalStatus, activeSessionId, loadMessages]);

  // Auto-scroll
  useEffect(() => {
    scrollRef.current?.scrollTo({
      top: scrollRef.current.scrollHeight,
      behavior: "smooth",
    });
  }, [messages, events]);

  const runStatus: RunStatus =
    (terminalStatus as RunStatus) ?? currentRun?.status ?? "queued";
  const isRunning =
    currentRun !== null && !terminalStatus;

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (!task.trim()) return;
    setSubmitting(true);

    try {
      const body: Record<string, string> = { task: task.trim(), profile };
      if (activeSessionId) body.session_id = activeSessionId;

      const sub = await apiPost<RunSubmission>("/v1/runs", body);
      setCurrentRun(sub);

      // Set active session to the one just created
      if (!activeSessionId) {
        setActiveSessionId(sub.session_id);
      }

      // Add user message locally for instant feedback
      setMessages((prev) => [
        ...prev,
        {
          id: `local:${Date.now()}`,
          session_id: sub.session_id,
          run_id: sub.run_id,
          role: "user",
          content: task.trim(),
          agent_role: null,
          model: null,
          timestamp: new Date().toISOString(),
        },
      ]);

      setTask("");
      // Refresh session list
      apiGet<SessionSummary[]>("/v1/sessions?limit=50")
        .then(setSessions)
        .catch(() => {});
    } catch (err) {
      console.error("submit:", err);
    } finally {
      setSubmitting(false);
    }
  }

  // Check which sessions have active runs
  const activeSessionIds = new Set(
    sessions
      .filter((s) => s.last_run_at && !s.last_task)
      .map((s) => s.session_id),
  );

  return (
    <div className="flex h-[calc(100vh-10rem)] gap-4">
      {/* Session sidebar */}
      <div className="w-56 shrink-0 overflow-y-auto rounded-xl border border-slate-200 bg-white">
        <div className="border-b border-slate-100 px-3 py-2">
          <h3 className="text-xs font-semibold uppercase tracking-wider text-slate-400">
            Sessions
          </h3>
        </div>
        <div className="p-1">
          <button
            onClick={() => {
              setActiveSessionId(null);
              setMessages([]);
              setCurrentRun(null);
            }}
            className={`mb-1 w-full rounded-lg px-3 py-2 text-left text-xs transition-colors ${
              activeSessionId === null
                ? "bg-teal-50 text-teal-700"
                : "text-slate-600 hover:bg-slate-50"
            }`}
          >
            + New Session
          </button>
          {sessions.map((s) => (
            <button
              key={s.session_id}
              onClick={() => {
                setActiveSessionId(s.session_id);
                setCurrentRun(null);
              }}
              className={`mb-0.5 w-full rounded-lg px-3 py-2 text-left text-xs transition-colors ${
                activeSessionId === s.session_id
                  ? "bg-teal-50 text-teal-700"
                  : "text-slate-600 hover:bg-slate-50"
              }`}
            >
              <div className="flex items-center gap-1.5">
                {activeSessionIds.has(s.session_id) && (
                  <span className="h-2 w-2 animate-pulse rounded-full bg-teal-500" />
                )}
                <span className="font-mono">
                  {s.session_id.slice(0, 8)}
                </span>
              </div>
              {s.last_task && (
                <div className="mt-0.5 truncate text-[10px] text-slate-400">
                  {s.last_task}
                </div>
              )}
            </button>
          ))}
        </div>
      </div>

      {/* Chat area */}
      <div className="flex flex-1 flex-col rounded-xl border border-slate-200 bg-white">
        {/* Messages */}
        <div
          ref={scrollRef}
          className="flex-1 space-y-3 overflow-y-auto px-4 py-4"
        >
          {messages.length === 0 && !isRunning && (
            <div className="flex h-full items-center justify-center">
              <p className="text-sm text-slate-400">
                {activeSessionId
                  ? "No messages yet. Send a task below."
                  : "Select a session or start a new conversation."}
              </p>
            </div>
          )}

          {messages.map((msg) => (
            <ChatBubble key={msg.id} message={msg} />
          ))}

          {isRunning && (
            <AgentThinking events={events} isRunning={isRunning} />
          )}

          {sseError && (
            <div className="rounded-lg border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-600">
              {sseError}
            </div>
          )}

          {terminalStatus && (
            <div className="flex justify-center py-1">
              <span
                className={`rounded-full px-3 py-1 text-xs ${
                  terminalStatus === "succeeded"
                    ? "bg-emerald-50 text-emerald-600"
                    : "bg-red-50 text-red-600"
                }`}
              >
                Run {terminalStatus}
              </span>
            </div>
          )}
        </div>

        {/* Input */}
        <form
          onSubmit={handleSubmit}
          className="flex items-end gap-2 border-t border-slate-100 px-4 py-3"
        >
          <div className="flex-1">
            <textarea
              value={task}
              onChange={(e) => setTask(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  handleSubmit(e);
                }
              }}
              placeholder="Describe a task..."
              rows={2}
              className="w-full resize-none rounded-lg border border-slate-200 bg-slate-50 px-3 py-2 text-sm placeholder:text-slate-400 focus:border-teal-500 focus:outline-none focus:ring-1 focus:ring-teal-500"
            />
          </div>
          <select
            value={profile}
            onChange={(e) => setProfile(e.target.value as TaskProfile)}
            className="rounded-lg border border-slate-200 bg-slate-50 px-2 py-2 text-xs focus:border-teal-500 focus:outline-none"
          >
            <option value="general">General</option>
            <option value="planning">Planning</option>
            <option value="extraction">Extraction</option>
            <option value="coding">Coding</option>
          </select>
          <button
            type="submit"
            disabled={submitting || !task.trim() || isRunning}
            className="rounded-lg bg-teal-600 px-4 py-2 text-sm font-medium text-white hover:bg-teal-700 disabled:opacity-50"
          >
            {submitting ? "..." : "Send"}
          </button>
        </form>
      </div>
    </div>
  );
}
