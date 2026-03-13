"use client";

import { useEffect, useRef, useCallback, useState } from "react";
import { useTerminalWs } from "@/hooks/use-terminal-ws";
import { apiDelete, apiGet, apiPost } from "@/lib/api-client";
import type { AppSettings, TerminalSessionInfo } from "@/lib/types";

interface TerminalPanelProps {
  terminalId?: string;
  runId?: string | null;
  sessionId?: string | null;
  defaultCommand?: string;
  defaultArgs?: string[];
  compact?: boolean;
  onSessionChange?: (id: string | null) => void;
}

function sortSessions(sessions: TerminalSessionInfo[]): TerminalSessionInfo[] {
  return [...sessions].sort(
    (left, right) =>
      new Date(right.created_at).getTime() - new Date(left.created_at).getTime(),
  );
}

function pickSessionId(
  sessions: TerminalSessionInfo[],
  preferredId?: string,
  runId?: string | null,
  sessionId?: string | null,
): string | null {
  if (preferredId && sessions.some((session) => session.id === preferredId)) {
    return preferredId;
  }
  if (runId) {
    const match = sessions.find((session) => session.run_id === runId);
    if (match) return match.id;
  }
  if (sessionId) {
    const match = sessions.find((session) => session.session_id === sessionId);
    if (match) return match.id;
  }
  return null;
}

function formatSessionLabel(session: TerminalSessionInfo): string {
  const command = [session.command, ...session.args].join(" ").trim();
  const scope = session.run_id
    ? `run ${session.run_id.slice(0, 8)}`
    : session.session_id
      ? `session ${session.session_id.slice(0, 8)}`
      : "manual";
  return `${scope} • ${command || session.id.slice(0, 8)}`;
}

export function TerminalPanel({
  terminalId: initialId,
  runId,
  sessionId,
  defaultCommand,
  defaultArgs,
  compact = false,
  onSessionChange,
}: TerminalPanelProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<any>(null);
  const initRef = useRef(false);

  const [activeId, setActiveId] = useState<string | null>(initialId ?? null);
  const [sessions, setSessions] = useState<TerminalSessionInfo[]>([]);
  const [terminalCommand, setTerminalCommand] = useState(defaultCommand ?? "claude");
  const [terminalArgs, setTerminalArgs] = useState<string[]>(defaultArgs ?? []);

  const setActiveSession = useCallback(
    (nextId: string | null) => {
      setActiveId((current) => {
        if (current === nextId) {
          return current;
        }
        onSessionChange?.(nextId);
        termRef.current?.reset?.();
        return nextId;
      });
    },
    [onSessionChange],
  );

  const onData = useCallback((data: Uint8Array) => {
    termRef.current?.write(data);
  }, []);

  const { sendInput, sendResize, connected, exitInfo, disconnect } =
    useTerminalWs(activeId, onData);

  const refreshSessions = useCallback(async () => {
    try {
      const list = sortSessions(
        await apiGet<TerminalSessionInfo[]>("/v1/terminal/sessions"),
      );
      setSessions(list);
      setActiveId((current) => {
        if (current && list.some((session) => session.id === current)) {
          return current;
        }
        const nextId = pickSessionId(list, initialId, runId, sessionId);
        if (current !== nextId) {
          onSessionChange?.(nextId);
        }
        return nextId;
      });
    } catch (err) {
      console.error("refresh terminal sessions:", err);
    }
  }, [initialId, onSessionChange, runId, sessionId]);

  useEffect(() => {
    if (initialId) {
      setActiveSession(initialId);
    }
  }, [initialId, setActiveSession]);

  useEffect(() => {
    void refreshSessions();
    const interval = window.setInterval(() => {
      void refreshSessions();
    }, 5000);
    return () => window.clearInterval(interval);
  }, [refreshSessions]);

  useEffect(() => {
    let cancelled = false;

    (async () => {
      try {
        const settings = await apiGet<AppSettings>("/v1/settings");
        if (cancelled) return;
        if (!defaultCommand) {
          setTerminalCommand(settings.terminal_command ?? "claude");
        }
        if (!defaultArgs) {
          setTerminalArgs(settings.terminal_args ?? []);
        }
      } catch (err) {
        console.error("load terminal defaults:", err);
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [defaultArgs, defaultCommand]);

  useEffect(() => {
    if (defaultCommand) {
      setTerminalCommand(defaultCommand);
    }
  }, [defaultCommand]);

  useEffect(() => {
    if (defaultArgs) {
      setTerminalArgs(defaultArgs);
    }
  }, [defaultArgs]);

  useEffect(() => {
    if (!containerRef.current || initRef.current) return;
    initRef.current = true;

    let disposed = false;

    (async () => {
      const { Terminal } = await import("@xterm/xterm");
      const { FitAddon } = await import("@xterm/addon-fit");
      const { WebLinksAddon } = await import("@xterm/addon-web-links");

      if (disposed || !containerRef.current) return;

      const term = new Terminal({
        cursorBlink: true,
        fontSize: 13,
        fontFamily: "'JetBrains Mono', 'Fira Code', 'Cascadia Code', monospace",
        theme: {
          background: "#1e1e2e",
          foreground: "#cdd6f4",
          cursor: "#f5e0dc",
          selectionBackground: "#585b7066",
        },
      });

      const fitAddon = new FitAddon();
      term.loadAddon(fitAddon);
      term.loadAddon(new WebLinksAddon());
      term.open(containerRef.current);
      fitAddon.fit();

      term.onData((data: string) => {
        sendInput(data);
      });

      term.onResize(({ cols, rows }: { cols: number; rows: number }) => {
        sendResize(cols, rows);
      });

      termRef.current = term;

      const ro = new ResizeObserver(() => {
        if (!disposed) fitAddon.fit();
      });
      ro.observe(containerRef.current);

      (containerRef.current as any).__cleanup = () => {
        disposed = true;
        ro.disconnect();
        term.dispose();
        termRef.current = null;
        initRef.current = false;
      };
    })();

    return () => {
      disposed = true;
      (containerRef.current as any)?.__cleanup?.();
    };
  }, [sendInput, sendResize]);

  async function spawnSession() {
    try {
      const response = await apiPost<{ id: string }>("/v1/terminal/sessions", {
        command: terminalCommand,
        args: terminalArgs,
        cols: termRef.current?.cols ?? 80,
        rows: termRef.current?.rows ?? 24,
        run_id: runId ?? undefined,
        session_id: sessionId ?? undefined,
      });
      setActiveSession(response.id);
      await refreshSessions();
    } catch (err) {
      termRef.current?.write(`\r\nFailed to spawn terminal: ${err}\r\n`);
    }
  }

  async function killSession() {
    if (!activeId) return;
    try {
      await apiDelete(`/v1/terminal/sessions/${activeId}`);
    } catch {
      // ignore kill errors
    }
    disconnect();
    setActiveSession(null);
    termRef.current?.write("\r\n[Session ended]\r\n");
    await refreshSessions();
  }

  return (
    <div
      className={`flex flex-col ${compact ? "h-80" : "h-[calc(100vh-12rem)]"}`}
    >
      <div className="flex flex-wrap items-center gap-2 border-b border-slate-700 bg-slate-800 px-3 py-1.5">
        <span
          className={`h-2 w-2 rounded-full ${connected ? "bg-green-400" : "bg-slate-500"}`}
        />
        <span className="text-xs text-slate-300">
          {connected ? `Connected: ${activeId?.slice(0, 8)}` : "Disconnected"}
        </span>
        <div className="min-w-[14rem] flex-1">
          <select
            value={activeId ?? ""}
            onChange={(event) =>
              setActiveSession(event.target.value || null)
            }
            className="w-full rounded border border-slate-600 bg-slate-900 px-2 py-1 text-xs text-slate-200"
          >
            <option value="">Select active terminal</option>
            {sessions.map((session) => (
              <option key={session.id} value={session.id}>
                {formatSessionLabel(session)}
              </option>
            ))}
          </select>
        </div>
        <button
          onClick={() => void refreshSessions()}
          className="rounded border border-slate-600 px-3 py-1 text-xs text-slate-200 hover:bg-slate-700"
        >
          Refresh
        </button>
        {!activeId && (
          <button
            onClick={() => void spawnSession()}
            className="rounded bg-teal-600 px-3 py-1 text-xs text-white hover:bg-teal-700"
          >
            New Terminal
          </button>
        )}
        {activeId && (
          <button
            onClick={() => void killSession()}
            className="rounded bg-red-600 px-3 py-1 text-xs text-white hover:bg-red-700"
          >
            Kill
          </button>
        )}
      </div>
      <div ref={containerRef} className="flex-1 bg-[#1e1e2e]" />
      {exitInfo && (
        <div className="bg-slate-800 px-3 py-1 text-xs text-slate-400">
          Process exited with code {exitInfo.code}
        </div>
      )}
    </div>
  );
}
