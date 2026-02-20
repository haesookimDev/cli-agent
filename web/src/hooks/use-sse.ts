"use client";

import { useEffect, useRef, useCallback, useState } from "react";
import { connectSSE } from "@/lib/sse";
import type { RunActionEvent } from "@/lib/types";

export function useRunSSE(runId: string | null) {
  const [events, setEvents] = useState<RunActionEvent[]>([]);
  const [terminalStatus, setTerminalStatus] = useState<string | null>(null);
  const controllerRef = useRef<AbortController | null>(null);

  const stop = useCallback(() => {
    controllerRef.current?.abort();
    controllerRef.current = null;
  }, []);

  useEffect(() => {
    if (!runId) return;
    setEvents([]);
    setTerminalStatus(null);

    const url = `/v1/runs/${runId}/stream?poll_ms=400&behavior=false`;

    connectSSE(url, (eventType, data) => {
      if (eventType === "action_event") {
        const ev = JSON.parse(data) as RunActionEvent;
        setEvents((prev) => [...prev, ev]);
      } else if (eventType === "run_terminal") {
        const ev = JSON.parse(data);
        setTerminalStatus(ev.status);
      }
    }).then((ctrl) => {
      controllerRef.current = ctrl;
    });

    return stop;
  }, [runId, stop]);

  return { events, terminalStatus, stop };
}
