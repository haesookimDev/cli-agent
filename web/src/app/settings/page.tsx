"use client";

import { useEffect, useState } from "react";
import { apiGet, apiPatch, apiPost } from "@/lib/api-client";
import type {
  AppSettings,
  CliModelBackend,
  ModelWithStatus,
} from "@/lib/types";

const CLI_BACKEND_OPTIONS: Array<{
  value: CliModelBackend;
  label: string;
  defaultCommand: string;
}> = [
  { value: "claude_code", label: "Claude Code", defaultCommand: "claude" },
  { value: "codex", label: "Codex CLI", defaultCommand: "codex" },
];

function defaultCommandForBackend(backend: CliModelBackend): string {
  return (
    CLI_BACKEND_OPTIONS.find((option) => option.value === backend)?.defaultCommand ??
    "claude"
  );
}

export default function SettingsPage() {
  const [models, setModels] = useState<ModelWithStatus[]>([]);
  const [loading, setLoading] = useState(true);

  const [cliModelEnabled, setCliModelEnabled] = useState(false);
  const [cliModelBackend, setCliModelBackend] =
    useState<CliModelBackend>("claude_code");
  const [cliModelCommand, setCliModelCommand] = useState("claude");
  const [cliModelArgs, setCliModelArgs] = useState("");
  const [cliModelTimeoutMs, setCliModelTimeoutMs] = useState("300000");
  const [cliModelOnly, setCliModelOnly] = useState(true);
  const [cliSaving, setCliSaving] = useState(false);

  const [terminalCommand, setTerminalCommand] = useState("claude");
  const [terminalArgs, setTerminalArgs] = useState("");
  const [terminalAutoSpawn, setTerminalAutoSpawn] = useState(false);
  const [terminalSaving, setTerminalSaving] = useState(false);

  async function loadPage() {
    try {
      const [loadedModels, settings] = await Promise.all([
        apiGet<ModelWithStatus[]>("/v1/models"),
        apiGet<AppSettings>("/v1/settings"),
      ]);
      setModels(loadedModels);
      setCliModelEnabled(settings.cli_model_enabled ?? false);
      setCliModelBackend(settings.cli_model_backend ?? "claude_code");
      setCliModelCommand(
        settings.cli_model_command ||
          defaultCommandForBackend(settings.cli_model_backend ?? "claude_code"),
      );
      setCliModelArgs((settings.cli_model_args ?? []).join(" "));
      setCliModelTimeoutMs(
        String(settings.cli_model_timeout_ms ?? 300_000),
      );
      setCliModelOnly(settings.cli_model_only ?? true);
      setTerminalCommand(settings.terminal_command ?? "claude");
      setTerminalArgs((settings.terminal_args ?? []).join(" "));
      setTerminalAutoSpawn(settings.terminal_auto_spawn ?? false);
    } catch (err) {
      console.error("load settings page:", err);
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    void loadPage();
  }, []);

  async function reloadModels() {
    try {
      const loaded = await apiGet<ModelWithStatus[]>("/v1/models");
      setModels(loaded);
    } catch (err) {
      console.error("reload models:", err);
    }
  }

  async function toggleModel(modelId: string) {
    try {
      await apiPost(`/v1/models/${encodeURIComponent(modelId)}/toggle`);
      await reloadModels();
    } catch (err) {
      console.error("toggle model:", err);
    }
  }

  async function toggleProvider(provider: string) {
    try {
      await apiPost(`/v1/providers/${encodeURIComponent(provider)}/toggle`);
      await reloadModels();
    } catch (err) {
      console.error("toggle provider:", err);
    }
  }

  async function setPreferred(modelId: string | null) {
    try {
      await apiPatch("/v1/settings", {
        preferred_model: modelId,
      } as Record<string, unknown>);
      await loadPage();
    } catch (err) {
      console.error("set preferred:", err);
    }
  }

  async function saveCliSettings() {
    setCliSaving(true);
    try {
      await apiPatch("/v1/settings", {
        cli_model_enabled: cliModelEnabled,
        cli_model_backend: cliModelEnabled ? cliModelBackend : null,
        cli_model_command: cliModelCommand.trim(),
        cli_model_args: cliModelArgs.trim()
          ? cliModelArgs.trim().split(/\s+/)
          : [],
        cli_model_timeout_ms: Number.parseInt(cliModelTimeoutMs, 10) || 300000,
        cli_model_only: cliModelOnly,
      });
      await loadPage();
    } catch (err) {
      console.error("save cli settings:", err);
    } finally {
      setCliSaving(false);
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
      await loadPage();
    } catch (err) {
      console.error("save terminal settings:", err);
    } finally {
      setTerminalSaving(false);
    }
  }

  function handleCliBackendChange(nextBackend: CliModelBackend) {
    const prevDefault = defaultCommandForBackend(cliModelBackend);
    const nextDefault = defaultCommandForBackend(nextBackend);
    setCliModelBackend(nextBackend);
    setCliModelCommand((current) => {
      if (!current.trim() || current === prevDefault) {
        return nextDefault;
      }
      return current;
    });
  }

  if (loading) {
    return (
      <div className="p-6 text-center text-sm text-slate-400">Loading...</div>
    );
  }

  const providers = [...new Set(models.map((m) => m.spec.provider))];
  const cliLockedProvider =
    cliModelEnabled && cliModelOnly
      ? cliModelBackend === "claude_code"
        ? "claude_code"
        : "codex"
      : null;

  return (
    <div className="space-y-6">
      <div className="rounded-xl border border-slate-200 bg-white p-5">
        <h2 className="mb-4 text-sm font-semibold text-slate-700">
          Model Catalog
        </h2>

        <div className="mb-4 flex flex-wrap gap-2">
          {providers.map((provider) => {
            const allDisabled = models
              .filter((m) => m.spec.provider === provider)
              .every((m) => !m.enabled);
            const locked = cliLockedProvider !== null && provider !== cliLockedProvider;
            return (
              <button
                key={provider}
                disabled={locked}
                onClick={() => toggleProvider(provider)}
                className={`rounded-full border px-3 py-1 text-xs font-medium transition-colors ${
                  allDisabled
                    ? "border-slate-200 bg-slate-50 text-slate-400"
                    : "border-teal-200 bg-teal-50 text-teal-700"
                } ${locked ? "cursor-not-allowed opacity-40" : ""}`}
              >
                {provider}
              </button>
            );
          })}
        </div>

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
              {models.map((model) => {
                const locked =
                  cliLockedProvider !== null &&
                  model.spec.provider !== cliLockedProvider;
                return (
                  <tr
                    key={model.spec.model_id}
                    className={`border-t border-slate-50 ${
                      !model.enabled ? "opacity-40" : ""
                    }`}
                  >
                    <td className="px-3 py-2 text-slate-500">
                      {model.spec.provider}
                    </td>
                    <td className="px-3 py-2 font-medium text-slate-700">
                      {model.spec.model_id}
                      {model.spec.local_only && (
                        <span className="ml-1 rounded bg-amber-100 px-1 py-0.5 text-[10px] text-amber-600">
                          local
                        </span>
                      )}
                    </td>
                    <td className="px-3 py-2 text-slate-500">
                      {(model.spec.quality * 100).toFixed(0)}%
                    </td>
                    <td className="px-3 py-2 text-slate-500">
                      {model.spec.latency.toFixed(0)}ms
                    </td>
                    <td className="px-3 py-2 text-slate-500">
                      ${model.spec.cost.toFixed(4)}
                    </td>
                    <td className="px-3 py-2 text-slate-500">
                      {(model.spec.context_window / 1000).toFixed(0)}k
                    </td>
                    <td className="px-3 py-2 text-center">
                      <button
                        disabled={locked}
                        onClick={() => toggleModel(model.spec.model_id)}
                        className={`inline-block h-5 w-9 rounded-full transition-colors ${
                          model.enabled ? "bg-teal-500" : "bg-slate-200"
                        } ${locked ? "cursor-not-allowed opacity-50" : ""}`}
                      >
                        <span
                          className={`block h-4 w-4 transform rounded-full bg-white shadow transition-transform ${
                            model.enabled ? "translate-x-4" : "translate-x-0.5"
                          }`}
                        />
                      </button>
                    </td>
                    <td className="px-3 py-2 text-center">
                      <input
                        type="radio"
                        name="preferred"
                        disabled={locked}
                        checked={model.is_preferred}
                        onChange={() =>
                          setPreferred(
                            model.is_preferred ? null : model.spec.model_id,
                          )
                        }
                        className="h-3.5 w-3.5 accent-teal-600 disabled:opacity-50"
                      />
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      </div>

      <div className="rounded-xl border border-slate-200 bg-white p-5">
        <h2 className="mb-4 text-sm font-semibold text-slate-700">AI CLI</h2>
        <div className="space-y-3">
          <div className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={cliModelEnabled}
              onChange={(event) => setCliModelEnabled(event.target.checked)}
              className="h-3.5 w-3.5 accent-teal-600"
            />
            <label className="text-xs text-slate-600">
              Use CLI provider for planner/reviewer/tool-caller/coder routing
            </label>
          </div>
          <div className="grid gap-3 md:grid-cols-2">
            <div>
              <label className="mb-1 block text-xs font-medium text-slate-500">
                Backend
              </label>
              <select
                value={cliModelBackend}
                disabled={!cliModelEnabled}
                onChange={(event) =>
                  handleCliBackendChange(event.target.value as CliModelBackend)
                }
                className="w-full rounded-lg border border-slate-200 bg-slate-50 px-3 py-2 text-sm focus:border-teal-500 focus:outline-none disabled:opacity-50"
              >
                {CLI_BACKEND_OPTIONS.map((option) => (
                  <option key={option.value} value={option.value}>
                    {option.label}
                  </option>
                ))}
              </select>
            </div>
            <div>
              <label className="mb-1 block text-xs font-medium text-slate-500">
                Timeout (ms)
              </label>
              <input
                type="number"
                min="1000"
                step="1000"
                value={cliModelTimeoutMs}
                disabled={!cliModelEnabled}
                onChange={(event) => setCliModelTimeoutMs(event.target.value)}
                className="w-full rounded-lg border border-slate-200 bg-slate-50 px-3 py-2 text-sm focus:border-teal-500 focus:outline-none disabled:opacity-50"
              />
            </div>
          </div>
          <div>
            <label className="mb-1 block text-xs font-medium text-slate-500">
              Command
            </label>
            <input
              type="text"
              value={cliModelCommand}
              disabled={!cliModelEnabled}
              onChange={(event) => setCliModelCommand(event.target.value)}
              className="w-full rounded-lg border border-slate-200 bg-slate-50 px-3 py-2 text-sm focus:border-teal-500 focus:outline-none disabled:opacity-50"
              placeholder={defaultCommandForBackend(cliModelBackend)}
            />
          </div>
          <div>
            <label className="mb-1 block text-xs font-medium text-slate-500">
              Arguments
            </label>
            <input
              type="text"
              value={cliModelArgs}
              disabled={!cliModelEnabled}
              onChange={(event) => setCliModelArgs(event.target.value)}
              className="w-full rounded-lg border border-slate-200 bg-slate-50 px-3 py-2 text-sm focus:border-teal-500 focus:outline-none disabled:opacity-50"
              placeholder="--full-auto"
            />
          </div>
          <div className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={cliModelOnly}
              disabled={!cliModelEnabled}
              onChange={(event) => setCliModelOnly(event.target.checked)}
              className="h-3.5 w-3.5 accent-teal-600"
            />
            <label className="text-xs text-slate-600">
              CLI-only mode: disable non-CLI providers at runtime
            </label>
          </div>
          <button
            onClick={saveCliSettings}
            disabled={cliSaving}
            className="rounded-lg bg-teal-600 px-4 py-2 text-sm font-medium text-white hover:bg-teal-700 disabled:opacity-50"
          >
            {cliSaving ? "Saving..." : "Save CLI"}
          </button>
        </div>
      </div>

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
              onChange={(event) => setTerminalCommand(event.target.value)}
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
              onChange={(event) => setTerminalArgs(event.target.value)}
              className="w-full rounded-lg border border-slate-200 bg-slate-50 px-3 py-2 text-sm focus:border-teal-500 focus:outline-none"
              placeholder="--dangerously-skip-permissions"
            />
          </div>
          <div className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={terminalAutoSpawn}
              onChange={(event) => setTerminalAutoSpawn(event.target.checked)}
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
            {terminalSaving ? "Saving..." : "Save Terminal"}
          </button>
        </div>
      </div>
    </div>
  );
}
