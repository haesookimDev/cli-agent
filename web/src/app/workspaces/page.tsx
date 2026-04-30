"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import { apiDelete, apiGet, apiPost } from "@/lib/api-client";
import { Workspace, WorkspaceKind } from "@/lib/types";

export default function WorkspacesPage() {
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [loading, setLoading] = useState(true);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [name, setName] = useState("");
  const [kind, setKind] = useState<WorkspaceKind>("general");
  const [rootPath, setRootPath] = useState("");
  const [description, setDescription] = useState("");

  const reload = useCallback(async () => {
    try {
      const list = await apiGet<Workspace[]>("/v1/workspaces");
      setWorkspaces(list);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to load workspaces");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    reload();
  }, [reload]);

  async function handleCreate(e: React.FormEvent) {
    e.preventDefault();
    if (!name.trim() || creating) return;
    setCreating(true);
    setError(null);
    try {
      await apiPost("/v1/workspaces", {
        name: name.trim(),
        kind,
        root_path: rootPath.trim() || undefined,
        description: description.trim() || undefined,
      });
      setName("");
      setRootPath("");
      setDescription("");
      await reload();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to create workspace");
    } finally {
      setCreating(false);
    }
  }

  async function handleDelete(ws: Workspace) {
    if (
      !window.confirm(
        `Delete workspace "${ws.name}"? Sessions stay alive but their workspace_id is cleared and the on-disk root is removed.`,
      )
    ) {
      return;
    }
    try {
      await apiDelete(`/v1/workspaces/${encodeURIComponent(ws.id)}`);
      await reload();
    } catch (e) {
      window.alert(e instanceof Error ? e.message : "Delete failed");
    }
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64 text-gray-500">
        Loading workspaces...
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <div>
        <h2 className="text-2xl font-bold text-slate-800">Workspaces</h2>
        <p className="text-sm text-slate-500 mt-1">
          Each workspace is a project folder that owns sessions and shared
          files. Agents run inside the workspace root, never the cli-agent
          repo.
        </p>
      </div>

      {error && (
        <div className="rounded border border-red-200 bg-red-50 px-3 py-2 text-sm text-red-700">
          {error}
        </div>
      )}

      <form
        onSubmit={handleCreate}
        className="rounded-xl border border-slate-200 bg-white p-4 space-y-3"
      >
        <h3 className="text-sm font-semibold text-slate-700">New workspace</h3>
        <div className="grid grid-cols-2 gap-3">
          <label className="block">
            <span className="text-xs text-slate-600 block mb-1">Name</span>
            <input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              required
              className="w-full px-2 py-1 border border-slate-300 rounded text-sm"
              placeholder="ProjectX"
            />
          </label>
          <label className="block">
            <span className="text-xs text-slate-600 block mb-1">Kind</span>
            <select
              value={kind}
              onChange={(e) => setKind(e.target.value as WorkspaceKind)}
              className="w-full px-2 py-1 border border-slate-300 rounded text-sm"
            >
              <option value="general">general</option>
              <option value="team">team</option>
            </select>
          </label>
        </div>
        <label className="block">
          <span className="text-xs text-slate-600 block mb-1">
            Root path (optional — defaults to ~/.cli-agent/workspaces/&lt;slug&gt;)
          </span>
          <input
            type="text"
            value={rootPath}
            onChange={(e) => setRootPath(e.target.value)}
            className="w-full px-2 py-1 border border-slate-300 rounded text-sm font-mono"
            placeholder="/Users/me/projects/projectx"
          />
        </label>
        <label className="block">
          <span className="text-xs text-slate-600 block mb-1">
            Description (optional)
          </span>
          <input
            type="text"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            className="w-full px-2 py-1 border border-slate-300 rounded text-sm"
          />
        </label>
        <div className="flex justify-end">
          <button
            type="submit"
            disabled={creating || !name.trim()}
            className="px-3 py-1.5 text-sm bg-blue-600 text-white rounded hover:bg-blue-700 disabled:bg-blue-300"
          >
            {creating ? "Creating..." : "Create workspace"}
          </button>
        </div>
      </form>

      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-4">
        {workspaces.length === 0 && (
          <div className="col-span-3 text-center py-8 text-slate-500 rounded-lg border border-dashed border-slate-300">
            No workspaces yet. Create one above.
          </div>
        )}
        {workspaces.map((ws) => (
          <div
            key={ws.id}
            className="rounded-xl border border-slate-200 bg-white p-4 hover:border-slate-400 transition-colors"
          >
            <div className="flex items-start justify-between gap-2">
              <div className="min-w-0">
                <Link
                  href={`/workspaces/${encodeURIComponent(ws.id)}`}
                  className="text-base font-semibold text-slate-800 hover:text-blue-700 truncate block"
                >
                  {ws.name}
                </Link>
                <p className="text-xs text-slate-400 mt-0.5">{ws.slug}</p>
              </div>
              <span
                className={`px-1.5 py-0.5 text-[10px] rounded font-medium ${
                  ws.kind === "team"
                    ? "bg-purple-100 text-purple-700"
                    : "bg-slate-100 text-slate-600"
                }`}
              >
                {ws.kind}
              </span>
            </div>
            <p className="text-xs text-slate-500 font-mono mt-2 truncate">
              {ws.root_path}
            </p>
            {ws.description && (
              <p className="text-xs text-slate-600 mt-2 line-clamp-2">
                {ws.description}
              </p>
            )}
            <div className="flex justify-end mt-3">
              <button
                onClick={() => handleDelete(ws)}
                className="px-2 py-0.5 text-xs text-red-600 hover:bg-red-50 rounded"
              >
                Delete
              </button>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
