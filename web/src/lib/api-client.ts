import { generateNonce, hmacSha256Hex } from "./hmac";

const API_KEY = process.env.NEXT_PUBLIC_API_KEY ?? "local-dev-key";
const API_SECRET = process.env.NEXT_PUBLIC_API_SECRET ?? "local-dev-secret";
const API_URL = process.env.NEXT_PUBLIC_API_URL ?? "http://localhost:8080";

async function authHeaders(
  rawBody: string = "",
): Promise<Record<string, string>> {
  const timestamp = Math.floor(Date.now() / 1000).toString();
  const nonce = generateNonce();
  const signature = await hmacSha256Hex(
    API_SECRET,
    `${timestamp}.${nonce}.${rawBody}`,
  );
  return {
    "X-API-Key": API_KEY,
    "X-Signature": signature,
    "X-Timestamp": timestamp,
    "X-Nonce": nonce,
  };
}

export async function apiGet<T>(path: string): Promise<T> {
  const headers = await authHeaders("");
  const resp = await fetch(`${API_URL}${path}`, { headers });
  if (!resp.ok) {
    const text = await resp.text();
    throw new Error(text || `HTTP ${resp.status}`);
  }
  return resp.json();
}

export async function apiPost<T>(path: string, body?: unknown): Promise<T> {
  const raw = body ? JSON.stringify(body) : "";
  const headers = await authHeaders(raw);
  headers["Content-Type"] = "application/json";
  const resp = await fetch(`${API_URL}${path}`, {
    method: "POST",
    headers,
    body: raw || undefined,
  });
  if (!resp.ok) {
    const text = await resp.text();
    throw new Error(text || `HTTP ${resp.status}`);
  }
  return resp.json();
}
