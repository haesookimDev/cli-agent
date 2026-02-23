"use client";

import { useEffect, useState } from "react";
import { apiGet, apiPatch, apiPost } from "@/lib/api-client";
import type { AppSettings, ModelWithStatus } from "@/lib/types";

export default function SettingsPage() {
  const [models, setModels] = useState<ModelWithStatus[]>([]);
  const [loading, setLoading] = useState(true);
  const [terminalCommand, setTerminalCommand] = useState("claude");
  const [terminalArgs, setTerminalArgs] = useState("");
  const [terminalAutoSpawn, setTerminalAutoSpawn] = useState(false);
  const [terminalSaving, setTerminalSaving] = useState(false);

  function loadModels() {
    apiGet<ModelWithStatus[]>("/v1/models")
      .then(setModels)
      .catch((err) => console.error("load models:", err))
      .finally(() => setLoading(false));
  }

  function loadTerminalSettings() {
    apiGet<AppSettings>("/v1/settings")
      .then((s) => {
        setTerminalCommand(s.terminal_command ?? "claude");
        setTerminalArgs((s.terminal_args ?? []).join(" "));
        setTerminalAutoSpawn(s.terminal_auto_spawn ?? false);
      })
      .catch((err) => console.error("load settings:", err));
  }

  useEffect(() => {
    loadModels();
    loadTerminalSettings();
  }, []);

  async function toggleModel(modelId: string) {
    try {
      await apiPost(`/v1/models/${encodeURIComponent(modelId)}/toggle`);
      loadModels();
    } catch (err) {
      console.error("toggle model:", err);
    }
  }

  async function toggleProvider(provider: string) {
    try {
      await apiPost(`/v1/providers/${encodeURIComponent(provider)}/toggle`);
      loadModels();
    } catch (err) {
      console.error("toggle provider:", err);
    }
  }

  async function setPreferred(modelId: string | null) {
    try {
      await apiPatch("/v1/settings", {
        preferred_model: modelId,
      } as Record<string, unknown>);
      loadModels();
    } catch (err) {
      console.error("set preferred:", err);
    }
  }

  async function saveTerminalSettings() {
    setTerminalSaving(true);
    try {
      await apiPatch("/v1/settings", {
        terminal_command: terminalCommand,
        terminal_args: terminalArgs.trim() ? terminalArgs.trim().split(/\s+/) : [],
        terminal_auto_spawn: terminalAutoSpawn,
      });
    } catch (err) {
      console.error("save terminal settings:", err);
    } finally {
      setTerminalSaving(false);
    }
  }

  if (loading) {
    return (
      <div className="p-6 text-center text-sm text-slate-400">Loading...</div>
    );
  }

  // Group by provider
  const providers = [...new Set(models.map((m) => m.spec.provider))];

  return (
    <div className="space-y-6">
      <div className="rounded-xl border border-slate-200 bg-white p-5">
        <h2 className="mb-4 text-sm font-semibold text-slate-700">
          Model Catalog
        </h2>

        {/* Provider toggles */}
        <div className="mb-4 flex flex-wrap gap-2">
          {providers.map((provider) => {
            const allDisabled = models
              .filter((m) => m.spec.provider === provider)
              .every((m) => !m.enabled);
            return (
              <button
                key={provider}
                onClick={() => toggleProvider(provider)}
                className={`rounded-full border px-3 py-1 text-xs font-medium transition-colors ${
                  allDisabled
                    ? "border-slate-200 bg-slate-50 text-slate-400"
                    : "border-teal-200 bg-teal-50 text-teal-700"
                }`}
              >
                {provider}
              </button>
            );
          })}
        </div>

        {/* Model table */}
        <div className="overflow-x-auto">
          <table className="w-full text-xs">
            <thead className="border-b border-slate-100 text-left text-slate-500">
              <tr>
                <th className="px-3 py-2 font-medium">Provider</th>
                <th className="px-3 py-2 font-medium">Model</th>
                <th className="px-3 py-2 font-medium">Quality</th>
                <th className="px-3 py-2 font-medium">Latency</th>
                <th className="px-3 py-2 font-medium">Cost</th>
                <th className="px-3 py-2 font-medium">Context</th>
                <th className="px-3 py-2 font-medium text-center">Enabled</th>
                <th className="px-3 py-2 font-medium text-center">Preferred</th>
              </tr>
            </thead>
            <tbody className="font-mono">
              {models.map((m) => (
                <tr
                  key={m.spec.model_id}
                  className={`border-t border-slate-50 ${
                    !m.enabled ? "opacity-40" : ""
                  }`}
                >
                  <td className="px-3 py-2 text-slate-500">
                    {m.spec.provider}
                  </td>
                  <td className="px-3 py-2 font-medium text-slate-700">
                    {m.spec.model_id}
                    {m.spec.local_only && (
                      <span className="ml-1 rounded bg-amber-100 px-1 py-0.5 text-[10px] text-amber-600">
                        local
                      </span>
                    )}
                  </td>
                  <td className="px-3 py-2 text-slate-500">
                    {(m.spec.quality * 100).toFixed(0)}%
                  </td>
                  <td className="px-3 py-2 text-slate-500">
                    {m.spec.latency.toFixed(0)}ms
                  </td>
                  <td className="px-3 py-2 text-slate-500">
                    ${m.spec.cost.toFixed(4)}
                  </td>
                  <td className="px-3 py-2 text-slate-500">
                    {(m.spec.context_window / 1000).toFixed(0)}k
                  </td>
                  <td className="px-3 py-2 text-center">
                    <button
                      onClick={() => toggleModel(m.spec.model_id)}
                      className={`inline-block h-5 w-9 rounded-full transition-colors ${
                        m.enabled ? "bg-teal-500" : "bg-slate-200"
                      }`}
                    >
                      <span
                        className={`block h-4 w-4 transform rounded-full bg-white shadow transition-transform ${
                          m.enabled ? "translate-x-4" : "translate-x-0.5"
                        }`}
                      />
                    </button>
                  </td>
                  <td className="px-3 py-2 text-center">
                    <input
                      type="radio"
                      name="preferred"
                      checked={m.is_preferred}
                      onChange={() =>
                        setPreferred(
                          m.is_preferred ? null : m.spec.model_id,
                        )
                      }
                      className="h-3.5 w-3.5 accent-teal-600"
                    />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>

      {/* Terminal Settings */}
      <div className="rounded-xl border border-slate-200 bg-white p-5">
        <h2 className="mb-4 text-sm font-semibold text-slate-700">
          Terminal
        </h2>
        <div className="space-y-3">
          <div>
            <label className="mb-1 block text-xs font-medium text-slate-500">
              Command
            </label>
            <input
              type="text"
              value={terminalCommand}
              onChange={(e) => setTerminalCommand(e.target.value)}
              className="w-full rounded-lg border border-slate-200 bg-slate-50 px-3 py-2 text-sm focus:border-teal-500 focus:outline-none"
              placeholder="claude"
            />
          </div>
          <div>
            <label className="mb-1 block text-xs font-medium text-slate-500">
              Arguments
            </label>
            <input
              type="text"
              value={terminalArgs}
              onChange={(e) => setTerminalArgs(e.target.value)}
              className="w-full rounded-lg border border-slate-200 bg-slate-50 px-3 py-2 text-sm focus:border-teal-500 focus:outline-none"
              placeholder="--dangerously-skip-permissions"
            />
          </div>
          <div className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={terminalAutoSpawn}
              onChange={(e) => setTerminalAutoSpawn(e.target.checked)}
              className="h-3.5 w-3.5 accent-teal-600"
            />
            <label className="text-xs text-slate-600">
              Auto-spawn terminal when Coder agent runs
            </label>
          </div>
          <button
            onClick={saveTerminalSettings}
            disabled={terminalSaving}
            className="rounded-lg bg-teal-600 px-4 py-2 text-sm font-medium text-white hover:bg-teal-700 disabled:opacity-50"
          >
            {terminalSaving ? "Saving..." : "Save"}
          </button>
        </div>
      </div>
    </div>
  );
}
