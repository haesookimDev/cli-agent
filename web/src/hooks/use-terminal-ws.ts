"use client";

import { useEffect, useRef, useCallback, useState } from "react";
import { generateNonce, hmacSha256Hex } from "@/lib/hmac";
import { API_KEY, API_SECRET, API_URL } from "@/lib/config";

export interface TerminalWsHandle {
  sendInput: (data: string | Uint8Array) => void;
  sendResize: (cols: number, rows: number) => void;
  connected: boolean;
  exitInfo: { code: number } | null;
  sessionError: string | null;
  disconnect: () => void;
}

export function useTerminalWs(
  terminalId: string | null,
  onData: (data: Uint8Array) => void,
): TerminalWsHandle {
  const wsRef = useRef<WebSocket | null>(null);
  const pendingInputRef = useRef<Uint8Array[]>([]);
  const [connected, setConnected] = useState(false);
  const [exitInfo, setExitInfo] = useState<{ code: number } | null>(null);
  const [sessionError, setSessionError] = useState<string | null>(null);
  const onDataRef = useRef(onData);
  onDataRef.current = onData;

  const disconnect = useCallback(() => {
    wsRef.current?.close();
    wsRef.current = null;
    pendingInputRef.current = [];
    setConnected(false);
  }, []);

  useEffect(() => {
    if (!terminalId) return;
    setExitInfo(null);
    setSessionError(null);
    pendingInputRef.current = [];

    let cancelled = false;
    let ws: WebSocket | null = null;

    (async () => {
      const timestamp = Math.floor(Date.now() / 1000).toString();
      const nonce = generateNonce();
      const signature = await hmacSha256Hex(
        API_SECRET,
        `${timestamp}.${nonce}.`,
      );

      // Effect was cleaned up while we were computing the signature.
      if (cancelled) return;

      const wsBase = API_URL.replace(/^http/, "ws");
      const params = new URLSearchParams({
        api_key: API_KEY,
        signature,
        timestamp,
        nonce,
      });
      const url = `${wsBase}/v1/terminal/sessions/${terminalId}/ws?${params}`;

      ws = new WebSocket(url);
      ws.binaryType = "arraybuffer";
      wsRef.current = ws;

      ws.onopen = () => {
        // Guard: effect may have been cleaned up between WS creation and open.
        if (cancelled) {
          ws?.close();
          return;
        }
        setConnected(true);
        setSessionError(null);
        for (const chunk of pendingInputRef.current) {
          ws!.send(chunk);
        }
        pendingInputRef.current = [];
      };
      ws.onclose = () => {
        // Only update state if this is still the active connection.
        // Prevents a stale onclose from nullifying a newer WS reference.
        if (wsRef.current === ws) {
          setConnected(false);
          wsRef.current = null;
        }
      };
      ws.onerror = () => {
        if (wsRef.current === ws) {
          setSessionError("terminal websocket connection failed");
        }
      };
      ws.onmessage = (ev) => {
        if (ev.data instanceof ArrayBuffer) {
          onDataRef.current(new Uint8Array(ev.data));
        } else if (typeof ev.data === "string") {
          try {
            const ctrl = JSON.parse(ev.data);
            if (ctrl.type === "exit") {
              setExitInfo({ code: ctrl.code ?? -1 });
            } else if (ctrl.type === "error") {
              setSessionError(
                typeof ctrl.message === "string" ? ctrl.message : "terminal error",
              );
            }
          } catch {
            // ignore non-JSON text
          }
        }
      };
    })();

    return () => {
      cancelled = true;
      ws?.close();
      wsRef.current = null;
      pendingInputRef.current = [];
      setConnected(false);
    };
  }, [terminalId]);

  const sendInput = useCallback((data: string | Uint8Array) => {
    const chunk =
      typeof data === "string" ? new TextEncoder().encode(data) : data;
    const ws = wsRef.current;
    if (!ws || ws.readyState !== WebSocket.OPEN) {
      pendingInputRef.current.push(chunk);
      return;
    }
    ws.send(chunk);
  }, []);

  const sendResize = useCallback((cols: number, rows: number) => {
    const ws = wsRef.current;
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    ws.send(JSON.stringify({ type: "resize", cols, rows }));
  }, []);

  return { sendInput, sendResize, connected, exitInfo, sessionError, disconnect };
}
