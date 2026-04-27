import { test, expect } from "@playwright/test";

const SETTINGS = {
  cli_model_enabled: false,
  cli_model_backend: "claude_code",
  cli_model_command: "claude",
  cli_model_args: [],
  cli_model_timeout_ms: 300000,
  cli_model_only: true,
  terminal_command: "bash",
  terminal_args: [],
  terminal_auto_spawn: false,
  vllm_base_url: "http://127.0.0.1:8000",
  vllm_custom_model: null,
  preferred_model: null,
};

const MODELS = [
  {
    spec: {
      provider: "anthropic",
      model_id: "claude-opus-4",
      quality: 95,
      latency: 1200,
      cost: 0.05,
      context_window: 200000,
    },
    enabled: true,
  },
  {
    spec: {
      provider: "anthropic",
      model_id: "claude-sonnet-4",
      quality: 90,
      latency: 800,
      cost: 0.02,
      context_window: 200000,
    },
    enabled: true,
  },
];

test.beforeEach(async ({ page }) => {
  await page.route("/v1/runs/active", (route) =>
    route.fulfill({ json: { runs: [] } }),
  );
  await page.route("/v1/models*", (route) => {
    if (route.request().method() === "GET") {
      route.fulfill({ json: MODELS });
    } else {
      route.fulfill({ json: { ok: true } });
    }
  });
  await page.route("/v1/settings*", (route) => {
    if (route.request().method() === "GET") {
      route.fulfill({ json: SETTINGS });
    } else {
      route.fulfill({ json: SETTINGS });
    }
  });
  await page.route("/v1/local-models*", (route) =>
    route.fulfill({ json: [] }),
  );
});

test("settings page renders the model catalog", async ({ page }) => {
  await page.goto("/settings");
  await expect(page.getByText("Model Catalog")).toBeVisible();
  await expect(page.getByText("claude-opus-4")).toBeVisible();
  await expect(page.getByText("claude-sonnet-4")).toBeVisible();
});

test("clicking a provider chip posts to /v1/providers/:name/toggle", async ({ page }) => {
  let toggleHits = 0;
  await page.route("/v1/providers/*/toggle", (route) => {
    if (route.request().method() === "POST") {
      toggleHits += 1;
      route.fulfill({ json: { ok: true } });
    } else {
      route.fulfill({ status: 405 });
    }
  });

  await page.goto("/settings");
  await page.getByRole("button", { name: "anthropic" }).click();
  await expect.poll(() => toggleHits).toBe(1);
});
