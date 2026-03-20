import { test, expect } from "@playwright/test";

test.beforeEach(async ({ page }) => {
  await page.route("/v1/runs/active", (route) =>
    route.fulfill({ json: { runs: [] } }),
  );
});

test("profile select has expected options", async ({ page }) => {
  await page.goto("/");
  const select = page.locator("select");
  await expect(select).toBeVisible();
  const options = await select.locator("option").allTextContents();
  expect(options).toContain("General");
  expect(options).toContain("Planning");
  expect(options).toContain("Extraction");
  expect(options).toContain("Coding");
});

test("submitting a task calls POST /v1/runs", async ({ page }) => {
  const runId = "00000000-0000-0000-0000-000000000001";
  const sessionId = "00000000-0000-0000-0000-000000000002";

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let requestBody: any = null;
  await page.route("/v1/runs", async (route) => {
    if (route.request().method() === "POST") {
      requestBody = JSON.parse(route.request().postData() ?? "{}");
      await route.fulfill({
        json: {
          run_id: runId,
          session_id: sessionId,
          status: "queued",
        },
      });
    } else {
      await route.fulfill({ json: { runs: [] } });
    }
  });
  // SSE stream for the run — return empty stream
  await page.route(`/v1/runs/${runId}/stream*`, (route) =>
    route.fulfill({ body: "", contentType: "text/event-stream" }),
  );

  await page.goto("/");
  await page.getByPlaceholder("Describe the task for the agent...").fill("refactor auth module");
  await page.getByRole("button", { name: "Run Task" }).click();

  await expect(page.getByText(runId.slice(0, 8))).toBeVisible();
  expect(requestBody?.task).toBe("refactor auth module");
  expect(requestBody?.profile).toBe("general");
});

test("error message is shown when API returns error", async ({ page }) => {
  await page.route("/v1/runs", (route) =>
    route.fulfill({ status: 500, body: "Internal Server Error" }),
  );

  await page.goto("/");
  await page.getByPlaceholder("Describe the task for the agent...").fill("trigger error");
  await page.getByRole("button", { name: "Run Task" }).click();

  await expect(page.locator(".text-red-700")).toBeVisible();
});
