"use client";

import { use, useCallback, useEffect, useState } from "react";
import Link from "next/link";
import { apiDelete, apiGet } from "@/lib/api-client";
import { API_KEY, API_SECRET, API_URL } from "@/lib/config";
import { generateNonce, hmacSha256Hex } from "@/lib/hmac";
import { setActiveWorkspaceId } from "@/lib/workspace-store";
import { Workspace, WorkspaceFile } from "@/lib/types";

interface PageProps {
  params: Promise<{ id: string }>;
}

export default function WorkspaceDetailPage({ params }: PageProps) {
  const { id: rawId } = use(params);
  const id = decodeURIComponent(rawId);

  const [ws, setWs] = useState<Workspace | null>(null);
  const [files, setFiles] = useState<WorkspaceFile[]>([]);
  const [sessions, setSessions] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [uploadPath, setUploadPath] = useState("");
  const [uploadBusy, setUploadBusy] = useState(false);

  const reload = useCallback(async () => {
    try {
      const [w, fs, sr] = await Promise.all([
        apiGet<Workspace>(`/v1/workspaces/${encodeURIComponent(id)}`),
        apiGet<WorkspaceFile[]>(
          `/v1/workspaces/${encodeURIComponent(id)}/files`,
        ).catch(() => []),
        apiGet<{ session_ids: string[] }>(
          `/v1/workspaces/${encodeURIComponent(id)}/sessions`,
        )
          .then((r) => r.session_ids)
          .catch(() => []),
      ]);
      setWs(w);
      setFiles(fs);
      setSessions(sr);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to load");
    } finally {
      setLoading(false);
    }
  }, [id]);

  useEffect(() => {
    reload();
  }, [reload]);

  // Pin this workspace as active for chats of the same kind so the chat
  // page can pick it up automatically next time.
  useEffect(() => {
    if (ws) setActiveWorkspaceId(ws.kind, ws.id).catch(() => {});
  }, [ws]);

  async function handleUpload(e: React.ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0];
    if (!file || !uploadPath.trim() || uploadBusy) return;
    setUploadBusy(true);
    try {
      const bytes = new Uint8Array(await file.arrayBuffer());
      const path = uploadPath.trim();
      const url = new URL(
        `${API_URL}/v1/workspaces/${encodeURIComponent(id)}/files`,
      );
      url.searchParams.set("path", path);
      const timestamp = Math.floor(Date.now() / 1000).toString();
      const nonce = generateNonce();
      // Sign over the *bytes* like apiPost does for JSON bodies.
      const decoder = new TextDecoder("utf-8");
      const rawBody = decoder.decode(bytes);
      const signature = await hmacSha256Hex(
        API_SECRET,
        `${timestamp}.${nonce}.${rawBody}`,
      );
      const resp = await fetch(url.toString(), {
        method: "POST",
        headers: {
          "X-API-Key": API_KEY,
          "X-Signature": signature,
          "X-Timestamp": timestamp,
          "X-Nonce": nonce,
          "Content-Type": file.type || "application/octet-stream",
        },
        body: bytes,
      });
      if (!resp.ok) {
        const text = await resp.text();
        throw new Error(text || `HTTP ${resp.status}`);
      }
      setUploadPath("");
      await reload();
    } catch (err) {
      window.alert(err instanceof Error ? err.message : "Upload failed");
    } finally {
      setUploadBusy(false);
      e.target.value = "";
    }
  }

  async function handleDeleteFile(path: string) {
    if (!window.confirm(`Delete ${path}?`)) return;
    try {
      await apiDelete(
        `/v1/workspaces/${encodeURIComponent(id)}/files?path=${encodeURIComponent(path)}`,
      );
      await reload();
    } catch (err) {
      window.alert(err instanceof Error ? err.message : "Delete failed");
    }
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64 text-gray-500">
        Loading...
      </div>
    );
  }
  if (error || !ws) {
    return (
      <div className="space-y-4">
        <Link href="/workspaces" className="text-sm text-blue-600 hover:underline">
          &larr; Back
        </Link>
        <div className="rounded border border-red-200 bg-red-50 p-4 text-sm text-red-700">
          {error ?? "Workspace not found"}
        </div>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <Link href="/workspaces" className="text-sm text-blue-600 hover:underline">
        &larr; Back to workspaces
      </Link>
      <div>
        <h2 className="text-2xl font-bold text-slate-800">{ws.name}</h2>
        <p className="text-xs text-slate-400 font-mono mt-1">{ws.root_path}</p>
        <p className="text-xs text-slate-500 mt-1">
          kind: {ws.kind} · slug: {ws.slug}
        </p>
        {ws.description && (
          <p className="text-sm text-slate-600 mt-2">{ws.description}</p>
        )}
      </div>

      <div>
        <h3 className="text-sm font-semibold text-slate-700 mb-2">
          Files ({files.length})
        </h3>
        <div className="rounded-xl border border-slate-200 bg-white p-3 mb-3">
          <div className="flex items-center gap-2">
            <input
              type="text"
              value={uploadPath}
              onChange={(e) => setUploadPath(e.target.value)}
              placeholder="notes/proposal.md"
              className="flex-1 px-2 py-1 border border-slate-300 rounded text-sm font-mono"
            />
            <label
              className={`px-3 py-1.5 text-sm rounded cursor-pointer ${
                uploadBusy || !uploadPath.trim()
                  ? "bg-slate-200 text-slate-500"
                  : "bg-blue-600 text-white hover:bg-blue-700"
              }`}
            >
              {uploadBusy ? "Uploading..." : "Upload"}
              <input
                type="file"
                className="hidden"
                onChange={handleUpload}
                disabled={uploadBusy || !uploadPath.trim()}
              />
            </label>
          </div>
          <p className="text-xs text-slate-500 mt-1">
            Path is workspace-relative. Use <code>notes/</code> prefix for
            shared markdown notes.
          </p>
        </div>
        {files.length === 0 ? (
          <div className="rounded border border-dashed border-slate-300 p-4 text-sm text-slate-500 text-center">
            No files yet.
          </div>
        ) : (
          <ul className="divide-y divide-slate-200 rounded border border-slate-200 bg-white">
            {files.map((f) => (
              <li key={f.id} className="px-3 py-2 flex items-center gap-3">
                <div className="flex-1 min-w-0">
                  <p className="text-sm font-mono text-slate-800 truncate">
                    {f.relative_path}
                  </p>
                  <p className="text-xs text-slate-400">
                    {f.size_bytes}B · {f.mime ?? "application/octet-stream"} ·
                    by {f.created_by}
                    {f.created_by_persona ? ` (${f.created_by_persona})` : ""}
                  </p>
                </div>
                <button
                  onClick={() => handleDeleteFile(f.relative_path)}
                  className="text-xs text-red-600 hover:bg-red-50 px-2 py-0.5 rounded"
                >
                  Delete
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>

      <div>
        <h3 className="text-sm font-semibold text-slate-700 mb-2">
          Sessions ({sessions.length})
        </h3>
        {sessions.length === 0 ? (
          <div className="text-sm text-slate-500">No sessions bound yet.</div>
        ) : (
          <ul className="divide-y divide-slate-200 rounded border border-slate-200 bg-white">
            {sessions.map((sid) => (
              <li key={sid} className="px-3 py-2">
                <Link
                  href={`/sessions/${encodeURIComponent(sid)}`}
                  className="text-sm text-blue-600 hover:underline font-mono"
                >
                  {sid}
                </Link>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
