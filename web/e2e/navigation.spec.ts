import { test, expect } from "@playwright/test";

// Mock the API endpoints so tests run without a backend
test.beforeEach(async ({ page }) => {
  await page.route("/v1/runs/active", (route) =>
    route.fulfill({ json: { runs: [] } }),
  );
  await page.route("/v1/runs*", (route) =>
    route.fulfill({ json: { runs: [] } }),
  );
  await page.route("/v1/sessions*", (route) =>
    route.fulfill({ json: { sessions: [] } }),
  );
  await page.route("/v1/memory*", (route) =>
    route.fulfill({ json: { memories: [] } }),
  );
  await page.route("/v1/workflows*", (route) =>
    route.fulfill({ json: { workflows: [] } }),
  );
  await page.route("/v1/schedules*", (route) =>
    route.fulfill({ json: { schedules: [] } }),
  );
  await page.route("/v1/settings*", (route) =>
    route.fulfill({ json: {} }),
  );
});

test("runner page loads with submit form", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByText("Submit Task")).toBeVisible();
  await expect(page.getByPlaceholder("Describe the task for the agent...")).toBeVisible();
  await expect(page.getByRole("button", { name: "Run Task" })).toBeVisible();
});

test("navigation tabs are visible", async ({ page }) => {
  await page.goto("/");
  const nav = page.locator("nav");
  await expect(nav.getByRole("link", { name: "Runner" })).toBeVisible();
  await expect(nav.getByRole("link", { name: "Sessions" })).toBeVisible();
  await expect(nav.getByRole("link", { name: "Runs" })).toBeVisible();
  await expect(nav.getByRole("link", { name: "Settings" })).toBeVisible();
});

test("run task button is disabled when task is empty", async ({ page }) => {
  await page.goto("/");
  const submitBtn = page.getByRole("button", { name: "Run Task" });
  await expect(submitBtn).toBeDisabled();
  await page.getByPlaceholder("Describe the task for the agent...").fill("test task");
  await expect(submitBtn).toBeEnabled();
});

test("navigates to runs page", async ({ page }) => {
  await page.goto("/");
  await page.locator("nav").getByRole("link", { name: "Runs" }).click();
  await expect(page).toHaveURL("/runs");
});

test("navigates to sessions page", async ({ page }) => {
  await page.goto("/");
  await page.locator("nav").getByRole("link", { name: "Sessions" }).click();
  await expect(page).toHaveURL("/sessions");
});
