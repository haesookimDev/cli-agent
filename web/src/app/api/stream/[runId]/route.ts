import { generateNonce, hmacSha256Hex } from "@/lib/hmac";

const API_KEY = process.env.NEXT_PUBLIC_API_KEY ?? "local-dev-key";
const API_SECRET = process.env.NEXT_PUBLIC_API_SECRET ?? "local-dev-secret";
const API_URL = process.env.NEXT_PUBLIC_API_URL ?? "http://localhost:8080";

export const dynamic = "force-dynamic";

export async function GET(
  request: Request,
  { params }: { params: Promise<{ runId: string }> },
) {
  const { runId } = await params;
  const { searchParams } = new URL(request.url);
  const pollMs = searchParams.get("poll_ms") ?? "400";
  const behavior = searchParams.get("behavior") ?? "false";

  const timestamp = Math.floor(Date.now() / 1000).toString();
  const nonce = generateNonce();
  const signature = await hmacSha256Hex(
    API_SECRET,
    `${timestamp}.${nonce}.`,
  );

  const upstream = await fetch(
    `${API_URL}/v1/runs/${runId}/stream?poll_ms=${pollMs}&behavior=${behavior}`,
    {
      headers: {
        "X-API-Key": API_KEY,
        "X-Signature": signature,
        "X-Timestamp": timestamp,
        "X-Nonce": nonce,
        Accept: "text/event-stream",
      },
    },
  );

  if (!upstream.ok || !upstream.body) {
    const text = await upstream.text();
    return new Response(text, { status: upstream.status });
  }

  return new Response(upstream.body, {
    headers: {
      "Content-Type": "text/event-stream",
      "Cache-Control": "no-cache",
      Connection: "keep-alive",
    },
  });
}
