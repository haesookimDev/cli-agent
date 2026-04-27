import { test, expect } from "@playwright/test";

const RUN_ID = "22222222-2222-2222-2222-222222222222";
const SESSION_ID = "33333333-3333-3333-3333-333333333333";

const RUN_RECORD = {
  run_id: RUN_ID,
  session_id: SESSION_ID,
  status: "running",
  profile: "general",
  task: "test cancellation",
  created_at: new Date().toISOString(),
  updated_at: new Date().toISOString(),
  timeline: [],
  output: null,
  error: null,
};

test.beforeEach(async ({ page }) => {
  await page.route("/v1/runs/active", (route) =>
    route.fulfill({ json: { runs: [] } }),
  );
});

test("Cancel button on a running run posts to /v1/runs/:id/cancel", async ({ page }) => {
  let cancelHits = 0;
  await page.route("/v1/runs?limit=50", (route) =>
    route.fulfill({ json: [RUN_RECORD] }),
  );
  await page.route(`/v1/runs/${RUN_ID}/cancel`, (route) => {
    if (route.request().method() === "POST") {
      cancelHits += 1;
      route.fulfill({ json: { status: "cancelling" } });
    } else {
      route.fulfill({ status: 405 });
    }
  });

  await page.goto("/runs");
  // RunActions exposes a Cancel button when status is "running" or "queued".
  await page.getByRole("button", { name: "Cancel" }).first().click();

  await expect.poll(() => cancelHits).toBe(1);
});
