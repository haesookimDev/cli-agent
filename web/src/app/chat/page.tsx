"use client";

import { Suspense, useEffect, useRef, useState, useCallback, useMemo, type ReactNode } from "react";
import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { apiGet, apiPost, apiDelete, apiPatch } from "@/lib/api-client";
import { useRunSSE } from "@/hooks/use-sse";
import { ChatBubble } from "@/components/chat-bubble";
import { AgentThinking } from "@/components/agent-thinking";
import { ToolCallCard, extractToolCalls } from "@/components/tool-call-card";
import { TerminalPanel } from "@/components/terminal-panel";
import { getLastSessionId, setLastSessionId } from "@/lib/session-store";
import type {
  ChatMessage,
  GlobalMemoryItem,
  SessionSummary,
  SessionMemoryItem,
  RunSubmission,
  RunActionEvent,
  RunTrace,
  TaskProfile,
} from "@/lib/types";

function ChatContent() {
  const searchParams = useSearchParams();
  const initialSessionId =
    searchParams.get("session") || getLastSessionId() || null;

  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [activeSessionId, setActiveSessionId] = useState<string | null>(
    initialSessionId,
  );
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [task, setTask] = useState("");
  const [profile, setProfile] = useState<TaskProfile>("general");
  const [submitting, setSubmitting] = useState(false);
  const submittingRef = useRef(false);
  const [showTerminal, setShowTerminal] = useState(false);
  const [sessionMemory, setSessionMemory] = useState<SessionMemoryItem[]>([]);
  const [globalMemory, setGlobalMemory] = useState<GlobalMemoryItem[]>([]);

  // Events per run (for timeline display — both SSE and API-loaded)
  const [runEventsMap, setRunEventsMap] = useState<Record<string, RunActionEvent[]>>({});
  const runEventsMapRef = useRef(runEventsMap);
  runEventsMapRef.current = runEventsMap;

  // Current active run
  const [currentRun, setCurrentRun] = useState<RunSubmission | null>(null);
  const { events, terminalStatus, sseError } = useRunSSE(
    currentRun?.run_id ?? null,
  );

  // Skip loadMessages when handleSubmit just created a new session
  const skipLoadRef = useRef(false);

  const scrollRef = useRef<HTMLDivElement>(null);

  // Sync activeSessionId to URL + sessionStorage
  useEffect(() => {
    setLastSessionId(activeSessionId);
    const url = new URL(window.location.href);
    if (activeSessionId) {
      url.searchParams.set("session", activeSessionId);
    } else {
      url.searchParams.delete("session");
    }
    window.history.replaceState(null, "", url.toString());
  }, [activeSessionId]);

  // Load sessions
  useEffect(() => {
    apiGet<SessionSummary[]>("/v1/sessions?limit=50")
      .then(setSessions)
      .catch(() => {});
  }, []);

  // Delete session
  const handleDeleteSession = useCallback(async (sid: string) => {
    if (!confirm("Delete this session?")) return;
    try {
      await apiDelete(`/v1/sessions/${sid}`);
      setSessions((prev) => prev.filter((s) => s.session_id !== sid));
      if (activeSessionId === sid) {
        setActiveSessionId(null);
        setMessages([]);
        setCurrentRun(null);
      }
    } catch (err) {
      console.error("delete session:", err);
    }
  }, [activeSessionId]);

  const loadGlobalMemory = useCallback(async () => {
    try {
      const items = await apiGet<GlobalMemoryItem[]>(
        "/v1/memory/global/items?limit=8",
      );
      setGlobalMemory(items);
    } catch (err) {
      console.error("loadGlobalMemory:", err);
    }
  }, []);

  const loadSessionMemory = useCallback(async (sid: string) => {
    try {
      const items = await apiGet<SessionMemoryItem[]>(
        `/v1/memory/sessions/${sid}/items?limit=8`,
      );
      setSessionMemory(items);
    } catch (err) {
      console.error("loadSessionMemory:", err);
      setSessionMemory([]);
    }
  }, []);

  useEffect(() => {
    loadGlobalMemory();
  }, [loadGlobalMemory]);

  useEffect(() => {
    if (activeSessionId) {
      loadSessionMemory(activeSessionId);
    } else {
      setSessionMemory([]);
    }
  }, [activeSessionId, loadSessionMemory]);

  // Load messages
  const loadMessages = useCallback(async (sid: string) => {
    try {
      const msgs = await apiGet<ChatMessage[]>(
        `/v1/sessions/${sid}/messages?limit=200`,
      );
      setMessages((prev) => {
        // Merge: keep only pending user messages that the server truly hasn't persisted yet.
        const pendingUserMsgs = prev.filter(
          (m) =>
            m.id.startsWith("pending:") &&
            m.role === "user" &&
            !msgs.some((serverMsg) => {
              if (serverMsg.role !== "user") return false;
              if (serverMsg.content !== m.content) return false;
              const serverTs = new Date(serverMsg.timestamp).getTime();
              const pendingTs = new Date(m.timestamp).getTime();
              return Math.abs(serverTs - pendingTs) <= 15_000;
            }),
        );
        if (pendingUserMsgs.length > 0) {
          // Insert pending messages at the right chronological position
          const merged = [...pendingUserMsgs, ...msgs];
          merged.sort(
            (a, b) => new Date(a.timestamp).getTime() - new Date(b.timestamp).getTime(),
          );
          return merged;
        }
        return msgs;
      });

      // Load events for runs we don't have yet
      const runIds = [...new Set(msgs.map((m) => m.run_id).filter(Boolean))] as string[];
      for (const rid of runIds) {
        if (runEventsMapRef.current[rid]) continue;
        apiGet<RunTrace>(`/v1/runs/${rid}/trace?limit=5000`)
          .then((trace) => {
            if (trace?.events?.length) {
              setRunEventsMap((prev) => ({ ...prev, [rid]: trace.events }));
            }
          })
          .catch(() => {});
      }
    } catch (err) {
      console.error("loadMessages:", err);
      // Don't clear messages on error — keep existing messages visible
    }
  }, []);

  // Load messages when session changes (skip when just created by handleSubmit)
  useEffect(() => {
    if (activeSessionId) {
      if (skipLoadRef.current) {
        skipLoadRef.current = false;
        return;
      }
      loadMessages(activeSessionId);
    } else {
      setMessages([]);
    }
  }, [activeSessionId, loadMessages]);

  // When run finishes: save events, reload messages
  useEffect(() => {
    if (terminalStatus && currentRun) {
      const runId = currentRun.run_id;
      setRunEventsMap((prev) => ({ ...prev, [runId]: events }));
      setCurrentRun(null);
      if (activeSessionId) {
        loadMessages(activeSessionId);
        loadSessionMemory(activeSessionId);
      }
      loadGlobalMemory();
    }
  }, [terminalStatus]); // eslint-disable-line react-hooks/exhaustive-deps

  // Auto-scroll
  useEffect(() => {
    scrollRef.current?.scrollTo({
      top: scrollRef.current.scrollHeight,
      behavior: "smooth",
    });
  }, [messages, events]);

  const isRunning = currentRun !== null && !terminalStatus;

  // Tool calls for runs WITHOUT events (fallback display)
  const toolCallsByRunId = useMemo(() => {
    const map: Record<string, ReturnType<typeof extractToolCalls>> = {};
    for (const [rid, evts] of Object.entries(runEventsMap)) {
      const calls = extractToolCalls(evts);
      if (calls.length > 0) map[rid] = calls;
    }
    return map;
  }, [runEventsMap]);

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (!task.trim() || submittingRef.current) return;
    submittingRef.current = true;
    setSubmitting(true);

    try {
      const body: Record<string, string> = { task: task.trim(), profile };
      if (activeSessionId) body.session_id = activeSessionId;

      const sub = await apiPost<RunSubmission>("/v1/runs", body);
      setCurrentRun(sub);

      if (!activeSessionId) {
        skipLoadRef.current = true; // Don't reload — we have the local message
        setActiveSessionId(sub.session_id);
      }

      setMessages((prev) => [
        ...prev,
        {
          id: `pending:${sub.run_id}`,
          session_id: sub.session_id,
          run_id: null,
          role: "user",
          content: task.trim(),
          agent_role: null,
          model: null,
          timestamp: new Date().toISOString(),
        },
      ]);

      setTask("");
      apiGet<SessionSummary[]>("/v1/sessions?limit=50")
        .then(setSessions)
        .catch(() => {});
    } catch (err) {
      console.error("submit:", err);
    } finally {
      submittingRef.current = false;
      setSubmitting(false);
    }
  }

  async function addSessionMemoryNote() {
    if (!activeSessionId) return;
    const content = prompt("세션 메모리 내용을 입력하세요");
    if (!content || !content.trim()) return;
    const scope = prompt("scope", "manual_note");
    const importanceInput = prompt("importance (0~1)", "0.6");
    const importance = Number(importanceInput ?? "0.6");
    if (!Number.isFinite(importance)) return;

    try {
      await apiPost(`/v1/memory/sessions/${activeSessionId}/items`, {
        content: content.trim(),
        scope: (scope ?? "manual_note").trim() || "manual_note",
        importance: Math.max(0, Math.min(1, importance)),
      });
      loadSessionMemory(activeSessionId);
    } catch (err) {
      console.error("addSessionMemoryNote:", err);
    }
  }

  async function editSessionMemoryItem(item: SessionMemoryItem) {
    const content = prompt("세션 메모리 수정", item.content);
    if (content === null || !content.trim()) return;
    const scope = prompt("scope", item.scope);
    if (scope === null || !scope.trim()) return;
    const importanceInput = prompt("importance (0~1)", String(item.importance));
    if (importanceInput === null) return;
    const importance = Number(importanceInput);
    if (!Number.isFinite(importance)) return;

    try {
      await apiPatch(`/v1/memory/items/${item.id}`, {
        content: content.trim(),
        scope: scope.trim(),
        importance: Math.max(0, Math.min(1, importance)),
      });
      if (activeSessionId) {
        loadSessionMemory(activeSessionId);
      }
    } catch (err) {
      console.error("editSessionMemoryItem:", err);
    }
  }

  async function addGlobalMemoryNote() {
    const topic = prompt("전역 메모리 topic", "manual");
    if (topic === null || !topic.trim()) return;
    const content = prompt("전역 메모리 content");
    if (content === null || !content.trim()) return;
    const importanceInput = prompt("importance (0~1)", "0.7");
    if (importanceInput === null) return;
    const importance = Number(importanceInput);
    if (!Number.isFinite(importance)) return;

    try {
      await apiPost("/v1/memory/global/items", {
        topic: topic.trim(),
        content: content.trim(),
        importance: Math.max(0, Math.min(1, importance)),
      });
      loadGlobalMemory();
    } catch (err) {
      console.error("addGlobalMemoryNote:", err);
    }
  }

  async function editGlobalMemoryItem(item: GlobalMemoryItem) {
    const topic = prompt("전역 메모리 topic 수정", item.topic);
    if (topic === null || !topic.trim()) return;
    const content = prompt("전역 메모리 content 수정", item.content);
    if (content === null || !content.trim()) return;
    const importanceInput = prompt("importance (0~1)", String(item.importance));
    if (importanceInput === null) return;
    const importance = Number(importanceInput);
    if (!Number.isFinite(importance)) return;

    try {
      await apiPatch(`/v1/memory/global/items/${item.id}`, {
        topic: topic.trim(),
        content: content.trim(),
        importance: Math.max(0, Math.min(1, importance)),
      });
      loadGlobalMemory();
    } catch (err) {
      console.error("editGlobalMemoryItem:", err);
    }
  }

  const activeSessionIds = new Set(
    sessions
      .filter((s) => s.last_run_at && !s.last_task)
      .map((s) => s.session_id),
  );

  // Build render items: group agent messages by run_id
  // If run has events → timeline card; else → ChatBubble
  const renderItems = useMemo(() => {
    const items: ReactNode[] = [];
    const renderedRuns = new Set<string>();

    for (let i = 0; i < messages.length; i++) {
      const msg = messages[i];

      // User messages → always ChatBubble
      if (msg.role === "user") {
        items.push(<ChatBubble key={msg.id} message={msg} />);
        continue;
      }

      // Agent message: check if run has events
      const rid = msg.run_id;

      // Already rendered this run as timeline → skip
      if (rid && renderedRuns.has(rid)) continue;

      if (rid && runEventsMap[rid] && runEventsMap[rid].length > 0) {
        // Has events → show timeline (only once per run)
        renderedRuns.add(rid);
        // Don't show timeline for current SSE run (handled separately below)
        if (rid !== currentRun?.run_id) {
          items.push(
            <AgentThinking
              key={`tl:${rid}`}
              events={runEventsMap[rid]}
              isRunning={false}
            />,
          );
        }
        continue;
      }

      // No events → show as ChatBubble with optional tool calls
      const nextMsg = messages[i + 1];
      const isLastAgentOfRun =
        rid &&
        (!nextMsg || nextMsg.run_id !== rid || nextMsg.role === "user");
      const runToolCalls =
        isLastAgentOfRun && rid ? toolCallsByRunId[rid] : null;

      items.push(
        <div key={msg.id}>
          <ChatBubble message={msg} />
          {runToolCalls && runToolCalls.length > 0 && (
            <div className="ml-9 mt-2 space-y-2">
              {runToolCalls.map((tc, ti) => (
                <ToolCallCard key={ti} call={tc} />
              ))}
            </div>
          )}
        </div>,
      );
    }

    // Current run timeline (active SSE)
    if (isRunning && currentRun && events.length > 0) {
      items.push(
        <AgentThinking
          key={`tl:${currentRun.run_id}`}
          events={events}
          isRunning={true}
        />,
      );
    }

    // Just-completed run (events saved, but messages might not include agent outputs yet)
    if (
      !isRunning &&
      terminalStatus &&
      currentRun === null &&
      events.length > 0
    ) {
      // The terminalStatus effect might not have fired yet
      // This handles the brief window between terminalStatus and loadMessages completing
    }

    return items;
  }, [messages, runEventsMap, currentRun, events, isRunning, terminalStatus, toolCallsByRunId]);

  return (
    <div className="flex h-full min-h-0 gap-4">
      {/* Session sidebar */}
      <div className="flex w-56 min-h-0 shrink-0 flex-col rounded-xl border border-slate-200 bg-white">
        <div className="border-b border-slate-100 px-3 py-2">
          <h3 className="text-xs font-semibold uppercase tracking-wider text-slate-400">
            Sessions
          </h3>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto p-1">
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
            <div
              key={s.session_id}
              className={`group relative mb-0.5 rounded-lg transition-colors ${
                activeSessionId === s.session_id
                  ? "bg-teal-50 text-teal-700"
                  : "text-slate-600 hover:bg-slate-50"
              }`}
            >
              <button
                onClick={() => {
                  setActiveSessionId(s.session_id);
                  setCurrentRun(null);
                }}
                className="w-full rounded-lg px-3 py-2 text-left text-xs"
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
                  <div className="mt-0.5 truncate pr-5 text-[10px] text-slate-400">
                    {s.last_task}
                  </div>
                )}
              </button>
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  handleDeleteSession(s.session_id);
                }}
                className="absolute right-1.5 top-1.5 hidden rounded p-0.5 text-slate-400 hover:bg-red-50 hover:text-red-500 group-hover:block"
                title="Delete session"
              >
                <svg xmlns="http://www.w3.org/2000/svg" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>
              </button>
            </div>
          ))}
        </div>

        {activeSessionId && (
          <div className="shrink-0 border-t border-slate-100 p-2">
            <div className="mb-1 flex items-center justify-between">
              <h4 className="text-[10px] font-semibold uppercase tracking-wider text-slate-400">
                Session Memory
              </h4>
              <button
                onClick={addSessionMemoryNote}
                className="rounded border border-slate-300 px-1.5 py-0.5 text-[10px] text-slate-600 hover:bg-slate-50"
              >
                Add
              </button>
            </div>
            <div className="max-h-40 space-y-1 overflow-y-auto pr-1">
              {sessionMemory.length === 0 ? (
                <div className="text-[10px] text-slate-400">No session memory</div>
              ) : (
                sessionMemory.map((item) => (
                  <button
                    key={item.id}
                    onClick={() => editSessionMemoryItem(item)}
                    className="w-full rounded border border-slate-200 bg-slate-50 p-2 text-left hover:bg-slate-100"
                  >
                    <div className="mb-0.5 flex items-center gap-1">
                      <span className="rounded bg-slate-200 px-1 py-0.5 text-[9px] text-slate-600">
                        {item.scope}
                      </span>
                      <span className="text-[9px] text-slate-400">
                        {item.importance.toFixed(2)}
                      </span>
                    </div>
                    <p className="line-clamp-2 whitespace-pre-wrap text-[10px] text-slate-700">
                      {item.content}
                    </p>
                  </button>
                ))
              )}
            </div>
          </div>
        )}

        <div className="shrink-0 border-t border-slate-100 p-2">
          <div className="mb-1 flex items-center justify-between">
            <h4 className="text-[10px] font-semibold uppercase tracking-wider text-slate-400">
              Global Memory
            </h4>
            <button
              onClick={addGlobalMemoryNote}
              className="rounded border border-slate-300 px-1.5 py-0.5 text-[10px] text-slate-600 hover:bg-slate-50"
            >
              Add
            </button>
          </div>
          <div className="max-h-40 space-y-1 overflow-y-auto pr-1">
            {globalMemory.length === 0 ? (
              <div className="text-[10px] text-slate-400">No global memory</div>
            ) : (
              globalMemory.map((item) => (
                <button
                  key={item.id}
                  onClick={() => editGlobalMemoryItem(item)}
                  className="w-full rounded border border-slate-200 bg-slate-50 p-2 text-left hover:bg-slate-100"
                >
                  <div className="mb-0.5 flex items-center gap-1">
                    <span className="rounded bg-teal-50 px-1 py-0.5 text-[9px] text-teal-700">
                      {item.topic}
                    </span>
                    <span className="text-[9px] text-slate-400">
                      {item.importance.toFixed(2)}
                    </span>
                  </div>
                  <p className="line-clamp-2 whitespace-pre-wrap text-[10px] text-slate-700">
                    {item.content}
                  </p>
                </button>
              ))
            )}
          </div>
          <Link
            href="/memory"
            className="mt-2 block text-[10px] text-teal-600 hover:underline"
          >
            Open full memory page
          </Link>
        </div>
      </div>

      {/* Chat area */}
      <div className="flex min-h-0 min-w-0 flex-1 flex-col rounded-xl border border-slate-200 bg-white">
        <div
          ref={scrollRef}
          className="min-h-0 flex-1 space-y-3 overflow-y-auto px-4 py-4"
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

          {renderItems}

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

        {/* Terminal panel (collapsible) */}
        {showTerminal && (
          <div className="shrink-0 border-t border-slate-200">
            <TerminalPanel compact={true} />
          </div>
        )}

        {/* Input */}
        <form
          onSubmit={handleSubmit}
          className="shrink-0 flex items-end gap-2 border-t border-slate-100 px-4 py-3"
        >
          <div className="flex-1">
            <textarea
              value={task}
              onChange={(e) => setTask(e.target.value)}
              onKeyDown={(e) => {
                if (
                  e.key === "Enter" &&
                  !e.shiftKey &&
                  !e.nativeEvent.isComposing
                ) {
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
            type="button"
            onClick={() => setShowTerminal(!showTerminal)}
            className={`rounded-lg border px-3 py-2 text-xs font-mono transition-colors ${
              showTerminal
                ? "border-teal-300 bg-teal-50 text-teal-700"
                : "border-slate-200 text-slate-600 hover:bg-slate-50"
            }`}
            title="Toggle terminal"
          >
            {">_"}
          </button>
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

export default function ChatPage() {
  return (
    <Suspense
      fallback={
        <div className="p-6 text-center text-sm text-slate-400">Loading...</div>
      }
    >
      <ChatContent />
    </Suspense>
  );
}
