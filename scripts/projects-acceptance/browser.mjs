import { createRequire } from "node:module";
import { resolve } from "node:path";
const [receiptPath, consoleRoot, outputRoot] = process.argv.slice(2);
if (!receiptPath || !consoleRoot || !outputRoot)
  throw new Error(
    "Usage: node browser.mjs RECEIPT CONSOLE_ROOT OUTPUT_DIRECTORY"
  );
const { chromium } = createRequire(resolve(consoleRoot, "package.json"))(
  "playwright"
);
import { readFileSync, writeFileSync } from "node:fs";
const receipt = JSON.parse(readFileSync(receiptPath, "utf8"));
if (receipt.origin !== "http://127.0.0.1:55440")
  throw new Error("Only the disposable loopback fixture is supported");
const browser = await chromium.launch({ headless: true });
const context = await browser.newContext();
const page = await context.newPage();
page.setDefaultTimeout(10000);
const errors = [];
page.on("pageerror", (e) => errors.push(e.message));
try {
  await page.goto(
    `${receipt.origin}/projects?organization_id=${receipt.organization_id}&issue=issue-public`
  );
  await page.getByRole("link", { name: "Sign in", exact: true }).click();
  await page.getByLabel("Email").fill("alice@example.test");
  await page.getByLabel("Password").fill("Local-acceptance-only-2026!");
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
  await page
    .locator("#issue-activity")
    .getByText(/Issue created/)
    .waitFor();
  if (await page.locator("#token").count())
    throw new Error("manual credential input remains");
  const begin = await (
    await context.request.post(`${receipt.origin}/auth/agent/connection/begin`)
  ).json();
  const recovery = await browser.newContext();
  const consent = await recovery.newPage();
  await consent.goto(begin.authorization_url);
  await consent.getByRole("link", { name: "Sign in", exact: true }).click();
  await consent.getByLabel("Email").fill("alice@example.test");
  await consent.getByLabel("Password").fill("Local-acceptance-only-2026!");
  await consent.getByRole("button", { name: "Sign in", exact: true }).click();
  await consent.getByRole("button", { name: "Allow connection" }).click();
  const grant = await (
    await context.request.post(`${receipt.origin}/auth/agent/connection/poll`, {
      data: {
        attempt_id: begin.attempt_id,
        polling_secret: begin.polling_secret,
      },
    })
  ).json();
  if (grant.grant.subject !== receipt.alice_subject)
    throw new Error("wrong identity");
  const api = await browser.newContext();
  const call = async (name, args) => {
    const res = await api.request.post(
      `${receipt.origin}/projects/agent/tools/execute`,
      {
        headers: { Authorization: `Bearer ${grant.grant.credential}` },
        data: { name, arguments_json: JSON.stringify(args) },
      }
    );
    return { status: res.status(), body: await res.json() };
  };
  const read = await call("projects_get_issue", {
    organization_id: receipt.organization_id,
    issue_ref: "issue-public",
  });
  if (read.status !== 200) throw new Error(`read ${read.status}`);
  const issue = JSON.parse(read.body.content);
  const states = await call("projects_list_issue_workflow_states", {
    organization_id: receipt.organization_id,
    team_id: issue.team_id,
    include_archived: false,
    limit: 100,
    after: null,
  });
  if (states.status !== 200) throw new Error(`states ${states.status}`);
  const catalog = JSON.parse(states.body.content);
  const done = catalog.items.find((x) => x.category === "completed");
  if (!done) throw new Error("missing completed workflow");
  const args = {
    organization_id: issue.organization_id,
    issue_id: issue.issue_id,
    idempotency_key: crypto.randomUUID(),
    expected_revision: issue.revision,
    title: issue.title,
    description: issue.description,
    priority: issue.priority,
    workflow_state_id: done.state_id,
    cycle_id: issue.cycle_id,
    milestone_id: issue.milestone_id,
    parent_issue_id: issue.parent_issue_id,
    label_ids: issue.label_ids,
  };
  const updated = await call("projects_update_issue", args);
  if (updated.status !== 200) throw new Error(`update ${updated.status}`);
  const saved = JSON.parse(updated.body.content);
  const conflict = await call("projects_update_issue", {
    ...args,
    idempotency_key: crypto.randomUUID(),
  });
  if (
    conflict.status !== 422 ||
    !JSON.stringify(conflict.body).includes("Read it again")
  )
    throw new Error("conflict not actionable");
  await page.getByRole("button", { name: "Refresh issue" }).click();
  await page.getByRole("button", { name: "Record details", exact: true }).click();
  await page.locator('[data-slot="description-list-item"]')
    .filter({ has: page.getByText("Version", { exact: true }) })
    .getByText(String(saved.revision), { exact: true }).waitFor();
  await page.getByRole("button", { name: "Record details", exact: true }).click();
  await page
    .locator("#issue-activity")
    .getByText(/Issue updated/)
    .waitFor();
  await page
    .locator("#issue-state")
    .getByText("Done", { exact: true })
    .waitFor();
  await page.getByRole("button", { name: /Issue updated/ }).last().click();
  await page.getByText(receipt.alice_subject, { exact: true }).last().waitFor();
  await page.getByRole("button", { name: /Issue updated/ }).last().click();
  if (errors.length) throw new Error(errors.join("\n"));
  const result = {
    passed: true,
    checks: [
      "cookie-login-return",
      "issue-deeplink",
      "activity-actor",
      "browser-consent",
      "consent-login-return",
      "workflow-update",
      "revision-conflict",
      "page-refresh",
    ],
    revision: saved.revision,
    state: saved.workflow_state_id,
    actor: grant.grant.subject,
  };
  writeFileSync(
    resolve(outputRoot, "browser-result.json"),
    JSON.stringify(result, null, 2)
  );
  console.log(JSON.stringify(result));
  await api.close();
} catch (error) {
  console.log((await page.locator("body").innerText()).slice(-2200));
  throw error;
} finally {
  await page.screenshot({
    path: resolve(outputRoot, "issue.png"),
    fullPage: true,
  });
  await browser.close();
}
