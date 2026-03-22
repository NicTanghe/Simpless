import { test, expect } from "@playwright/test";

test("homepage has title and heading text", async ({ page }) => {
  await page.goto("http://localhost:3001/");

  await expect(page).toHaveTitle("simpless control deck");

  await expect(page.locator("h1")).toHaveText("A scrolling ops deck for the activator.");
});
