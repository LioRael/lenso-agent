import { createRequire } from "node:module";
import { resolve } from "node:path";
import { readFileSync, mkdirSync, writeFileSync } from "node:fs";
const [receiptPath, consoleRoot, outputRoot] = process.argv.slice(2);
if (!receiptPath || !consoleRoot || !outputRoot)
  throw Error("Usage: node console-browser.mjs RECEIPT CONSOLE_ROOT OUTPUT");
const { chromium } = createRequire(resolve(consoleRoot, "package.json"))("playwright");
const receipt = JSON.parse(readFileSync(receiptPath, "utf8"));
if (receipt.origin !== "http://127.0.0.1:55440")
  throw Error("Only the disposable acceptance App is supported");
const origin = "http://127.0.0.1:55450";
mkdirSync(outputRoot, { recursive: true });
const browser = await chromium.launch({ headless: true });
const context = await browser.newContext({
  viewport: { width: 1440, height: 1000 },
  colorScheme: "light",
});
const page = await context.newPage();
const errors = [];
page.on("pageerror", (e) => errors.push(e.message));
page.setDefaultTimeout(10000);
try {
  await context.request.post(
    `${origin}/api/console/v1/pages/projects/services/projects/invoke/disconnect`,
    { data: {} },
  );
  await page.goto(
    `${origin}/workspaces/projects/org/${receipt.organization_id}/issues/issue-public`,
  );
  await page.getByRole("button", { name: "Connect account", exact: true }).click();
  const popupPromise = context.waitForEvent("page");
  await page.getByRole("link", { name: "Continue in browser" }).click();
  const consent = await popupPromise;
  await consent.getByRole("link", { name: "Sign in", exact: true }).click();
  await consent.getByLabel("Email").fill("alice@example.test");
  await consent.getByLabel("Password").fill("Local-acceptance-only-2026!");
  await consent.getByRole("button", { name: "Sign in", exact: true }).click();
  await consent.getByRole("button", { name: "Allow connection" }).click();
  await page.locator("#issue-state").waitFor();

  await page.screenshot({ path: resolve(outputRoot, "issue-light.png") });
  await page.emulateMedia({ colorScheme: "dark" });
  await page
    .locator(".projects-workspace")
    .first()
    .evaluate(async (element) => {
      for (let i = 0; i < 50; i++) {
        if (element.closest("[data-theme]")?.getAttribute("data-theme") === "dark") return;
        await new Promise((r) => setTimeout(r, 20));
      }
      throw Error("Workspace did not follow Console theme");
    });
  await page.screenshot({ path: resolve(outputRoot, "issue-dark.png") });
  await page.emulateMedia({ colorScheme: "light" });
  if (await page.locator("iframe").count()) throw Error("Workspace must be native");
  await page.evaluate(() => {
    window.__projectsAcceptanceDocument = true;
  });
  await page.getByRole("link", { name: "Projects", exact: true }).last().click();
  await page.getByRole("button", { name: "New project", exact: true }).waitFor();
  if (!(await page.evaluate(() => window.__projectsAcceptanceDocument)))
    throw Error("Workspace navigation reloaded Console");

  await page
    .locator(".project-list")
    .getByRole("button")
    .filter({ hasText: "public project" })
    .first()
    .waitFor();
  await page.screenshot({ path: resolve(outputRoot, "projects.png") });
  await page.getByRole("button", { name: "New project", exact: true }).click();
  await page.getByRole("dialog").waitFor();
  await page
    .getByRole("textbox", { name: "Name", exact: true })
    .fill("Console workspace acceptance");
  await page.getByRole("combobox", { name: "Lead team" }).click();

  await page.getByRole("option").first().click();
  await page.screenshot({ path: resolve(outputRoot, "create-project.png") });
  await page.getByRole("button", { name: "Create project", exact: true }).click();
  await page.getByRole("dialog").waitFor({ state: "hidden" });
  await page
    .getByRole("button", { name: /Console workspace acceptance/ })
    .first()
    .waitFor();
  await page.locator("[data-agent-tray]").click();
  await page.getByText("Welcome to Lenso", { exact: true }).waitFor();
  await page.screenshot({ path: resolve(outputRoot, "mini-agent.png") });
  const denied = await context.request.post(
    `${origin}/api/console/v1/pages/projects/services/projects/invoke/get_issue`,
    { data: { organization_id: receipt.other_organization_id, issue_id: "issue-public" } },
  );
  const deniedBody = await denied.json();
  if (![403, 404].includes(deniedBody.status))
    throw Error("Cross-organization access was not denied");
  const badTarget = await context.request.post(
    `${origin}/api/console/v1/pages/projects/services/projects/invoke/get_issue`,
    {
      data: {
        organization_id: receipt.organization_id,
        issue_id: "issue-public",
        origin: "https://example.com",
      },
    },
  );
  if (badTarget.status() !== 422 || (await badTarget.json()).code !== "workspace_service_rejected")
    throw Error("Client could retarget the business App");
  const secretFree = await (
    await context.request.post(
      `${origin}/api/console/v1/pages/projects/services/projects/invoke/connection_status`,
      { data: {} },
    )
  ).json();
  if ("credential" in secretFree || "polling_secret" in secretFree)
    throw Error("Credential crossed into browser");
  if (errors.length) throw Error(errors.join("\n"));
  writeFileSync(
    resolve(outputRoot, "result.json"),
    JSON.stringify(
      {
        passed: true,
        checks: [
          "real Console Shell",
          "native shared React module",
          "business App login and consent",
          "issue read and activity",
          "navigation preserves document",
          "project creation through business authorization",
          "shared dialog and select",
          "existing mini agent",
          "cross-organization denial",
          "fixed App destination",
          "no browser credentials",
        ],
        errors,
      },
      null,
      2,
    ),
  );
  console.log("Console Projects acceptance passed");
} catch (e) {
  console.log((await page.locator("body").innerText()).slice(0, 5000));
  await page.screenshot({ path: resolve(outputRoot, "failure.png") });
  throw e;
} finally {
  await browser.close();
}
