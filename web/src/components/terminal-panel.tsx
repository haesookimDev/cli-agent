"use client";

import { useEffect, useRef, useCallback, useState, type FormEvent } from "react";
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

const SHELL_COMMAND = "bash";
const SHELL_ARGS: string[] = [];

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

function normalizeCommand(command: string): string {
  return command.replace(/\\/g, "/").split("/").pop()?.toLowerCase() ?? command.toLowerCase();
}

function isShellCommand(command: string): boolean {
  return [
    "bash",
    "zsh",
    "sh",
    "fish",
    "pwsh",
    "powershell",
    "powershell.exe",
    "cmd",
    "cmd.exe",
  ].includes(normalizeCommand(command));
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
  const sessionErrorRef = useRef<string | null>(null);
  const terminalInputRef = useRef<(data: string) => void>(() => {});
  const pendingInputRef = useRef("");
  const spawningShellRef = useRef(false);

  const [activeId, setActiveId] = useState<string | null>(initialId ?? null);
  const [sessions, setSessions] = useState<TerminalSessionInfo[]>([]);
  const [terminalCommand, setTerminalCommand] = useState(defaultCommand ?? SHELL_COMMAND);
  const [terminalArgs, setTerminalArgs] = useState<string[]>(defaultArgs ?? []);
  const [commandInput, setCommandInput] = useState("");

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

  const { sendInput, sendResize, connected, exitInfo, sessionError, disconnect } =
    useTerminalWs(activeId, onData);
  const activeSession = sessions.find((session) => session.id === activeId) ?? null;
  const configuredCommandLabel = [terminalCommand, ...terminalArgs].join(" ").trim();
  const configuredSessionIsShell = isShellCommand(terminalCommand);
  const shellCommand = configuredSessionIsShell ? terminalCommand : SHELL_COMMAND;
  const shellArgs = configuredSessionIsShell ? terminalArgs : SHELL_ARGS;

  const focusTerminal = useCallback(() => {
    termRef.current?.focus?.();
  }, []);

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
          requestAnimationFrame(() => {
            termRef.current?.focus?.();
          });
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
          setTerminalCommand(settings.terminal_command ?? SHELL_COMMAND);
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
      term.focus();

      term.onData((data: string) => {
        terminalInputRef.current(data);
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

  useEffect(() => {
    if (!activeId) return;
    requestAnimationFrame(() => {
      focusTerminal();
    });
  }, [activeId, focusTerminal]);

  useEffect(() => {
    if (!connected) return;
    // Sync terminal dimensions with the PTY on every (re)connection.
    if (termRef.current) {
      sendResize(termRef.current.cols, termRef.current.rows);
    }
    if (pendingInputRef.current) {
      sendInput(pendingInputRef.current);
      pendingInputRef.current = "";
    }
    requestAnimationFrame(() => {
      focusTerminal();
    });
  }, [connected, focusTerminal, sendInput, sendResize]);

  useEffect(() => {
    if (!sessionError) {
      sessionErrorRef.current = null;
      return;
    }
    if (sessionErrorRef.current === sessionError) {
      return;
    }
    sessionErrorRef.current = sessionError;
    termRef.current?.write(`\r\n[Terminal error] ${sessionError}\r\n`);
  }, [sessionError]);

  async function spawnSession(command: string, args: string[]) {
    try {
      const response = await apiPost<{ id: string }>("/v1/terminal/sessions", {
        command,
        args,
        cols: termRef.current?.cols ?? 80,
        rows: termRef.current?.rows ?? 24,
        run_id: runId ?? undefined,
        session_id: sessionId ?? undefined,
      });
      setActiveSession(response.id);
      requestAnimationFrame(() => {
        focusTerminal();
      });
      await refreshSessions();
      return true;
    } catch (err) {
      termRef.current?.write(`\r\nFailed to spawn terminal: ${err}\r\n`);
      return false;
    }
  }

  async function spawnConfiguredSession() {
    await spawnSession(terminalCommand, terminalArgs);
  }

  async function spawnShellSession() {
    await spawnSession(shellCommand, shellArgs);
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

  async function queueInputForShell(data: string) {
    pendingInputRef.current += data;
    if (activeId || spawningShellRef.current) {
      return;
    }

    spawningShellRef.current = true;
    const spawned = await spawnSession(shellCommand, shellArgs);
    spawningShellRef.current = false;

    if (!spawned) {
      pendingInputRef.current = "";
    }
  }

  async function handleTerminalInput(data: string) {
    if (activeId) {
      sendInput(data);
      return;
    }
    await queueInputForShell(data);
  }

  terminalInputRef.current = (data: string) => {
    void handleTerminalInput(data);
  };

  async function handleCommandSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!commandInput.trim()) {
      focusTerminal();
      return;
    }

    const line = commandInput.endsWith("\n") ? commandInput : `${commandInput}\n`;
    setCommandInput("");

    if (activeId) {
      sendInput(line);
      focusTerminal();
      return;
    }

    await queueInputForShell(line);
    focusTerminal();
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
        <button
          onClick={() => void spawnShellSession()}
          className="rounded bg-teal-600 px-3 py-1 text-xs text-white hover:bg-teal-700"
        >
          New Shell
        </button>
        {!configuredSessionIsShell && (
          <button
            onClick={() => void spawnConfiguredSession()}
            className="rounded border border-slate-600 px-3 py-1 text-xs text-slate-200 hover:bg-slate-700"
          >
            New Configured
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
      {activeSession && (
        <div className="border-b border-slate-700 bg-slate-900 px-3 py-1 text-[11px] text-slate-400">
          Active: {formatSessionLabel(activeSession)}
          {!isShellCommand(activeSession.command) && (
            <span className="ml-2 text-amber-300">
              Non-shell session. Use `New Shell` for direct command entry.
            </span>
          )}
        </div>
      )}
      {!activeSession && configuredCommandLabel && !configuredSessionIsShell && (
        <div className="border-b border-slate-700 bg-slate-900 px-3 py-1 text-[11px] text-slate-400">
          Configured terminal: {configuredCommandLabel}
          <span className="ml-2 text-amber-300">
            Use `New Shell` to run shell commands directly.
          </span>
        </div>
      )}
      <div
        ref={containerRef}
        className="flex-1 bg-[#1e1e2e]"
        onMouseDown={() => focusTerminal()}
      />
      <form
        onSubmit={handleCommandSubmit}
        className="flex items-center gap-2 border-t border-slate-700 bg-slate-900 px-3 py-2"
      >
        <input
          value={commandInput}
          onChange={(event) => setCommandInput(event.target.value)}
          placeholder={
            activeSession
              ? isShellCommand(activeSession.command)
                ? "Run a shell command or send input"
                : "Send input to the active terminal process"
              : "Start a shell and run a command"
          }
          className="flex-1 rounded border border-slate-600 bg-slate-950 px-3 py-1.5 text-xs text-slate-100 placeholder:text-slate-500 focus:border-teal-500 focus:outline-none"
        />
        <button
          type="submit"
          disabled={!commandInput.trim()}
          className="rounded bg-teal-600 px-3 py-1.5 text-xs text-white hover:bg-teal-700 disabled:cursor-not-allowed disabled:bg-slate-700 disabled:text-slate-400"
        >
          {activeId ? "Send" : "Start Shell & Send"}
        </button>
      </form>
      {exitInfo && (
        <div className="bg-slate-800 px-3 py-1 text-xs text-slate-400">
          Process exited with code {exitInfo.code}
        </div>
      )}
    </div>
  );
}
