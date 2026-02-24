"use client";

import { useCallback, useEffect, useState } from "react";
import { apiGet, apiPatch, apiPost } from "@/lib/api-client";
import type { GlobalMemoryItem } from "@/lib/types";

export default function MemoryPage() {
  const [items, setItems] = useState<GlobalMemoryItem[]>([]);
  const [query, setQuery] = useState("");
  const [loading, setLoading] = useState(true);
  const [topic, setTopic] = useState("");
  const [content, setContent] = useState("");
  const [importance, setImportance] = useState("0.7");
  const [saving, setSaving] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const trimmed = query.trim();
      const path = trimmed
        ? `/v1/memory/global/items?limit=200&query=${encodeURIComponent(trimmed)}`
        : "/v1/memory/global/items?limit=200";
      const data = await apiGet<GlobalMemoryItem[]>(path);
      setItems(data);
    } catch (err) {
      console.error("load global memory:", err);
    } finally {
      setLoading(false);
    }
  }, [query]);

  useEffect(() => {
    load();
  }, [load]);

  async function handleCreate(e: React.FormEvent) {
    e.preventDefault();
    const trimmedTopic = topic.trim();
    const trimmedContent = content.trim();
    if (!trimmedTopic || !trimmedContent) return;

    setSaving(true);
    try {
      await apiPost("/v1/memory/global/items", {
        topic: trimmedTopic,
        content: trimmedContent,
        importance: Number.isFinite(Number(importance))
          ? Math.min(1, Math.max(0, Number(importance)))
          : 0.7,
      });
      setTopic("");
      setContent("");
      setImportance("0.7");
      await load();
    } catch (err) {
      console.error("create global memory:", err);
    } finally {
      setSaving(false);
    }
  }

  async function handleEdit(item: GlobalMemoryItem) {
    const nextTopic = prompt("Topic", item.topic);
    if (nextTopic === null) return;
    const nextContent = prompt("Content", item.content);
    if (nextContent === null) return;
    const nextImportance = prompt("Importance (0~1)", String(item.importance));
    if (nextImportance === null) return;

    const parsedImportance = Number(nextImportance);
    if (!Number.isFinite(parsedImportance)) return;

    try {
      await apiPatch(`/v1/memory/global/items/${item.id}`, {
        topic: nextTopic.trim(),
        content: nextContent.trim(),
        importance: Math.min(1, Math.max(0, parsedImportance)),
      });
      await load();
    } catch (err) {
      console.error("update global memory:", err);
    }
  }

  return (
    <div className="space-y-4">
      <div className="rounded-xl border border-slate-200 bg-white p-4">
        <h2 className="text-sm font-semibold text-slate-700">Global Memory</h2>
        <p className="mt-1 text-xs text-slate-500">
          All sessions share these knowledge items.
        </p>

        <form onSubmit={handleCreate} className="mt-4 grid gap-2">
          <input
            value={topic}
            onChange={(e) => setTopic(e.target.value)}
            placeholder="Topic"
            className="rounded-lg border border-slate-200 bg-slate-50 px-3 py-2 text-sm"
          />
          <textarea
            value={content}
            onChange={(e) => setContent(e.target.value)}
            placeholder="Content"
            rows={3}
            className="rounded-lg border border-slate-200 bg-slate-50 px-3 py-2 text-sm"
          />
          <div className="flex items-center gap-2">
            <input
              value={importance}
              onChange={(e) => setImportance(e.target.value)}
              placeholder="0.7"
              className="w-24 rounded-lg border border-slate-200 bg-slate-50 px-3 py-2 text-sm"
            />
            <button
              type="submit"
              disabled={saving}
              className="rounded-lg bg-teal-600 px-3 py-2 text-xs font-medium text-white hover:bg-teal-700 disabled:opacity-50"
            >
              {saving ? "Saving..." : "Add"}
            </button>
          </div>
        </form>
      </div>

      <div className="rounded-xl border border-slate-200 bg-white p-4">
        <div className="mb-3 flex items-center gap-2">
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search topic/content..."
            className="flex-1 rounded-lg border border-slate-200 bg-slate-50 px-3 py-2 text-sm"
          />
          <button
            onClick={load}
            className="rounded-lg border border-slate-300 px-3 py-2 text-xs text-slate-600 hover:bg-slate-50"
          >
            Refresh
          </button>
        </div>

        {loading ? (
          <div className="py-8 text-center text-sm text-slate-400">Loading...</div>
        ) : items.length === 0 ? (
          <div className="py-8 text-center text-sm text-slate-400">
            No global memory items
          </div>
        ) : (
          <div className="space-y-2">
            {items.map((item) => (
              <div
                key={item.id}
                className="rounded-lg border border-slate-200 bg-slate-50 p-3"
              >
                <div className="mb-1 flex items-center gap-2">
                  <span className="rounded bg-teal-50 px-2 py-0.5 text-[10px] text-teal-700">
                    {item.topic}
                  </span>
                  <span className="text-[10px] text-slate-400">
                    importance {item.importance.toFixed(2)}
                  </span>
                  <span className="text-[10px] text-slate-400">
                    access {item.access_count}
                  </span>
                  <button
                    onClick={() => handleEdit(item)}
                    className="ml-auto rounded border border-slate-300 px-2 py-0.5 text-[10px] text-slate-600 hover:bg-slate-100"
                  >
                    Edit
                  </button>
                </div>
                <p className="line-clamp-3 whitespace-pre-wrap text-xs text-slate-700">
                  {item.content}
                </p>
                <div className="mt-1 text-[10px] text-slate-400">
                  {new Date(item.updated_at).toLocaleString()}
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
