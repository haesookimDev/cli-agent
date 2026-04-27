import { test, expect } from "@playwright/test";

const SESSION_ID = "11111111-1111-1111-1111-111111111111";

test.beforeEach(async ({ page }) => {
  await page.route("/v1/runs/active", (route) =>
    route.fulfill({ json: { runs: [] } }),
  );
});

test("sessions page renders empty state when API returns nothing", async ({ page }) => {
  await page.route("/v1/sessions*", (route) => {
    if (route.request().method() === "GET") {
      route.fulfill({ json: [] });
    } else {
      route.fulfill({ json: { session_id: SESSION_ID } });
    }
  });

  await page.goto("/sessions");
  await expect(page.getByText("No sessions yet")).toBeVisible();
});

test("clicking New Session calls POST /v1/sessions", async ({ page }) => {
  let postCount = 0;
  await page.route("/v1/sessions*", (route) => {
    if (route.request().method() === "POST") {
      postCount += 1;
      route.fulfill({ json: { session_id: SESSION_ID } });
    } else {
      // First GET returns empty, subsequent GETs return the new session
      route.fulfill({
        json:
          postCount === 0
            ? []
            : [
                {
                  session_id: SESSION_ID,
                  created_at: new Date().toISOString(),
                  run_count: 0,
                  last_run_at: null,
                  last_task: null,
                },
              ],
      });
    }
  });

  await page.goto("/sessions");
  await page.getByRole("button", { name: "New Session" }).click();

  await expect.poll(() => postCount).toBe(1);
  await expect(page.locator("a", { hasText: SESSION_ID.slice(0, 8) })).toBeVisible();
});

test("clicking a session row navigates to its detail page", async ({ page }) => {
  await page.route("/v1/sessions*", (route) =>
    route.fulfill({
      json: [
        {
          session_id: SESSION_ID,
          created_at: new Date().toISOString(),
          run_count: 1,
          last_run_at: new Date().toISOString(),
          last_task: "build feature X",
        },
      ],
    }),
  );
  await page.route(`/v1/sessions/${SESSION_ID}*`, (route) =>
    route.fulfill({
      json: {
        session_id: SESSION_ID,
        created_at: new Date().toISOString(),
        run_count: 1,
        last_run_at: new Date().toISOString(),
        last_task: "build feature X",
      },
    }),
  );

  await page.goto("/sessions");
  await page.locator("a", { hasText: SESSION_ID.slice(0, 8) }).click();
  await expect(page).toHaveURL(`/sessions/${SESSION_ID}`);
});
