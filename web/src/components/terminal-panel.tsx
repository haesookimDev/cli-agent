"use client";

import { useEffect, useRef, useCallback, useState } from "react";
import { useTerminalWs } from "@/hooks/use-terminal-ws";
import { apiPost, apiDelete } from "@/lib/api-client";

interface TerminalPanelProps {
  terminalId?: string;
  defaultCommand?: string;
  defaultArgs?: string[];
  compact?: boolean;
  onSessionChange?: (id: string | null) => void;
}

export function TerminalPanel({
  terminalId: initialId,
  defaultCommand = "claude",
  defaultArgs = [],
  compact = false,
  onSessionChange,
}: TerminalPanelProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<any>(null);
  const fitRef = useRef<any>(null);
  const [activeId, setActiveId] = useState<string | null>(initialId ?? null);
  const initRef = useRef(false);

  const onData = useCallback((data: Uint8Array) => {
    termRef.current?.write(data);
  }, []);

  const { sendInput, sendResize, connected, exitInfo, disconnect } =
    useTerminalWs(activeId, onData);

  // Initialize xterm.js (dynamic import to avoid SSR)
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
      fitRef.current = fitAddon;

      const ro = new ResizeObserver(() => {
        if (!disposed) fitAddon.fit();
      });
      ro.observe(containerRef.current);

      // Store cleanup
      (containerRef.current as any).__cleanup = () => {
        disposed = true;
        ro.disconnect();
        term.dispose();
        termRef.current = null;
        fitRef.current = null;
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
      const resp = await apiPost<{ id: string }>("/v1/terminal/sessions", {
        command: defaultCommand,
        args: defaultArgs,
        cols: termRef.current?.cols ?? 80,
        rows: termRef.current?.rows ?? 24,
      });
      setActiveId(resp.id);
      onSessionChange?.(resp.id);
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
    setActiveId(null);
    onSessionChange?.(null);
    termRef.current?.write("\r\n[Session ended]\r\n");
  }

  return (
    <div
      className={`flex flex-col ${compact ? "h-80" : "h-[calc(100vh-12rem)]"}`}
    >
      {/* Toolbar */}
      <div className="flex items-center gap-2 border-b border-slate-700 bg-slate-800 px-3 py-1.5">
        <span
          className={`h-2 w-2 rounded-full ${connected ? "bg-green-400" : "bg-slate-500"}`}
        />
        <span className="text-xs text-slate-300">
          {connected ? `Connected: ${activeId?.slice(0, 8)}` : "Disconnected"}
        </span>
        <div className="flex-1" />
        {!activeId && (
          <button
            onClick={spawnSession}
            className="rounded bg-teal-600 px-3 py-1 text-xs text-white hover:bg-teal-700"
          >
            New Terminal
          </button>
        )}
        {activeId && (
          <button
            onClick={killSession}
            className="rounded bg-red-600 px-3 py-1 text-xs text-white hover:bg-red-700"
          >
            Kill
          </button>
        )}
      </div>
      {/* Terminal container */}
      <div ref={containerRef} className="flex-1 bg-[#1e1e2e]" />
      {exitInfo && (
        <div className="bg-slate-800 px-3 py-1 text-xs text-slate-400">
          Process exited with code {exitInfo.code}
        </div>
      )}
    </div>
  );
}
