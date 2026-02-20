import { generateNonce, hmacSha256Hex } from "./hmac";

const API_KEY = process.env.NEXT_PUBLIC_API_KEY ?? "local-dev-key";
const API_SECRET = process.env.NEXT_PUBLIC_API_SECRET ?? "local-dev-secret";
const API_URL = process.env.NEXT_PUBLIC_API_URL ?? "http://localhost:8080";

export type SSEHandler = (eventType: string, data: string) => void;

export async function connectSSE(
  path: string,
  onEvent: SSEHandler,
  onError?: (err: Error) => void,
): Promise<AbortController> {
  const controller = new AbortController();

  const timestamp = Math.floor(Date.now() / 1000).toString();
  const nonce = generateNonce();
  const signature = await hmacSha256Hex(
    API_SECRET,
    `${timestamp}.${nonce}.`,
  );

  try {
    const resp = await fetch(`${API_URL}${path}`, {
      headers: {
        "X-API-Key": API_KEY,
        "X-Signature": signature,
        "X-Timestamp": timestamp,
        "X-Nonce": nonce,
        Accept: "text/event-stream",
      },
      signal: controller.signal,
    });

    if (!resp.ok || !resp.body) {
      onError?.(new Error(`SSE failed: ${resp.status}`));
      return controller;
    }

    const reader = resp.body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";

    (async () => {
      try {
        for (;;) {
          const { done, value } = await reader.read();
          if (done) break;
          buffer += decoder.decode(value, { stream: true });

          const parts = buffer.split("\n\n");
          buffer = parts.pop() ?? "";

          for (const part of parts) {
            let eventType = "message";
            let data = "";
            for (const line of part.split("\n")) {
              if (line.startsWith("event:"))
                eventType = line.slice(6).trim();
              else if (line.startsWith("data:"))
                data = line.slice(5).trim();
            }
            if (data) onEvent(eventType, data);
          }
        }
      } catch (err) {
        if ((err as Error).name !== "AbortError") {
          onError?.(err as Error);
        }
      }
    })();
  } catch (err) {
    if ((err as Error).name !== "AbortError") {
      onError?.(err as Error);
    }
  }

  return controller;
}
