// Run against a disposable API database and the static image with a matching config.json.
const assert = require("node:assert/strict");
const { randomUUID } = require("node:crypto");
const { mkdir } = require("node:fs/promises");
const { chromium } = require("playwright");

(async () => {
  const base = process.env.DASHBOARD_TEST_URL;
  const signupCode = process.env.DASHBOARD_SIGNUP_CODE;
  assert.ok(base && signupCode, "DASHBOARD_TEST_URL and DASHBOARD_SIGNUP_CODE are required");
  const browser = await chromium.launch({ headless: true, args: ["--mute-audio"] });
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("dialog", (dialog) => dialog.accept());
  const email = `browser-${randomUUID()}@example.test`;
  const password = "가나다라";
  const screenshots = process.env.DASHBOARD_SCREENSHOTS;
  try {
    await page.goto(base);
    await page.locator("#login-form").waitFor({ state: "visible" });
    assert.equal(await page.locator("html").getAttribute("lang"), "en");
    assert.equal(await page.title(), "llmproxy · Dashboard");
    if (screenshots) {
      await mkdir(screenshots, { recursive: true });
      await page.screenshot({ path: `${screenshots}/login.png`, fullPage: true });
    }
    await page.setViewportSize({ width: 390, height: 844 });
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
    if (screenshots) await page.screenshot({ path: `${screenshots}/mobile-login.png`, fullPage: true });
    await page.setViewportSize({ width: 1440, height: 1000 });
    const { apiUrl } = await (await context.request.get(`${base}/config.json`)).json();
    assert.notEqual(new URL(base).origin, new URL(apiUrl).origin, "exercise cross-origin cookies");
    await page.locator("#signup-tab").click();
    await page.locator("#signup-label").fill("Personal workspace");
    await page.locator("#signup-email").fill(email);
    await page.locator("#signup-password").fill(password);
    await page.locator("#signup-code").fill(signupCode);
    await page.locator("#signup-form button[type=submit]").click();
    await page.locator("#app").waitFor({ state: "visible" });
    await page.waitForFunction(() => document.getElementById("summary-requests").textContent === "0");
    const session = (await context.cookies(`${apiUrl}/management`)).find((cookie) => cookie.name === "llmproxy_session");
    assert.ok(session?.httpOnly);
    assert.equal(session.sameSite, "Strict");
    assert.equal(session.path, "/management");
    assert.equal(await page.locator("#signup-password").inputValue(), "");
    assert.equal(await page.locator("#signup-code").inputValue(), "");

    await page.locator('nav [data-view="tokens"]').click();
    const tokens = [];
    const labels = ['Development <img src=x onerror="window.__xss=true">', "Production app"];
    for (const label of labels) {
      await page.locator("#token-label").fill(label);
      const response = page.waitForResponse((response) => response.url() === `${apiUrl}/management/api-tokens` && response.request().method() === "POST");
      await page.locator("#token-form button[type=submit]").click();
      const issued = await (await response).json();
      tokens.push(issued);
      await page.locator("#token-dialog").waitFor({ state: "visible" });
      assert.equal(await page.locator("#issued-token").inputValue(), issued.token);
      await page.locator('#token-dialog button[data-close="token-dialog"]').last().click();
      await page.waitForFunction(() => document.getElementById("issued-token").value === "");
    }
    await page.waitForFunction(() => document.querySelectorAll("#token-list tbody tr").length === 2);
    assert.equal(await page.evaluate(() => window.__xss), undefined);
    assert.equal(await page.locator("#token-list img").count(), 0);
    await page.locator("#metrics-token").selectOption(tokens[0].id);
    await page.waitForFunction(() => document.querySelector("#metrics-content .metric-item dd")?.textContent === "0");
    assert.equal(await page.evaluate(() => localStorage.length + sessionStorage.length), 0);

    await page.locator('nav [data-view="providers"]').click();
    const providers = [];
    for (const [kind, label] of [["codex", "Personal Codex"], ["claude", "Personal Claude"]]) {
      await page.locator("#provider-kind").selectOption(kind);
      await page.locator("#provider-label").fill(label);
      const response = page.waitForResponse((response) => response.url() === `${apiUrl}/management/providers` && response.request().method() === "POST");
      await page.locator("#provider-form button[type=submit]").click();
      providers.push(await (await response).json());
      await page.locator(`.provider-card[data-provider-id="${providers.at(-1).id}"]`).waitFor();
    }
    const codex = page.locator(`.provider-card[data-provider-id="${providers[0].id}"]`);
    await codex.locator('[name="label"]').fill("Primary Codex");
    await codex.locator('[name="enabled"]').uncheck();
    await codex.locator('button[type="submit"]').click();
    await codex.getByText("Provider settings saved.").waitFor();
    let saved = await (await context.request.get(`${apiUrl}/management/providers/${providers[0].id}`)).json();
    assert.equal(saved.label, "Primary Codex");
    assert.equal(saved.enabled, false);
    await codex.locator('[name="enabled"]').check();
    const enabled = page.waitForResponse((response) => response.url().endsWith(`/providers/${providers[0].id}`) && response.request().method() === "PATCH");
    await codex.locator('button[type="submit"]').click();
    assert.equal((await (await enabled).json()).enabled, true);

    // UI protocol fixtures never contact a subscription provider.
    let polls = 0;
    let submittedCode;
    const pending = { id: "fixture-login", status: "pending", expires_at: new Date(Date.now() + 60000).toISOString(), interval_seconds: 1, next_poll_at: new Date().toISOString() };
    await page.route(`${apiUrl}/management/providers/*/logins**`, async (route) => {
      const request = route.request();
      const url = request.url();
      const isClaude = url.includes(providers[1].id);
      let body;
      if (url.endsWith("/logins")) {
        body = { ...pending, authorization_url: "https://example.test/authorize", user_code: "TEST-CODE" };
      } else if (isClaude) {
        submittedCode = request.postDataJSON()?.code;
        body = { ...pending, status: "completed" };
      } else {
        polls += 1;
        body = { ...pending, status: polls < 2 ? "pending" : "completed" };
      }
      await route.fulfill({ status: 200, contentType: "application/json", headers: { "Access-Control-Allow-Origin": new URL(base).origin, "Access-Control-Allow-Credentials": "true" }, body: JSON.stringify(body) });
    });
    await codex.locator('[data-action="login"]').click();
    await page.locator("#device-code").getByText("TEST-CODE").waitFor();
    await page.getByText("Sign-in complete. Credentials saved.").waitFor();
    assert.equal(polls, 2);
    await page.locator('#oauth-dialog button[data-close="oauth-dialog"]').last().click();
    const claude = page.locator(`.provider-card[data-provider-id="${providers[1].id}"]`);
    await claude.locator('[data-action="login"]').click();
    await page.locator("#oauth-code").fill("test-code#test-state");
    await page.locator("#oauth-code-form button[type=submit]").click();
    await page.getByText("Sign-in complete. Credentials saved.").waitFor();
    assert.equal(submittedCode, "test-code#test-state");
    await page.locator('#oauth-dialog button[data-close="oauth-dialog"]').last().click();
    assert.equal(await page.locator("#oauth-code").inputValue(), "");
    await page.unroute(`${apiUrl}/management/providers/*/logins**`);

    await page.locator('nav [data-view="routing"]').click();
    await page.locator("#routing-strategy").selectOption("priority");
    await page.locator("#add-route").click();
    const first = page.locator(".route-row").first();
    await first.locator('[name="model"]').fill("chat");
    await first.locator('[name="provider_id"]').selectOption(providers[0].id);
    await first.locator('[name="upstream_model"]').fill("test-model");
    await first.locator('[name="priority"]').fill("2");
    await page.locator("#routing-form button[type=submit]").click();
    await page.getByText("Model routes saved.").waitFor();
    await page.locator("#add-route").click();
    const second = page.locator(".route-row").last();
    await second.locator('[name="model"]').fill("chat");
    await second.locator('[name="provider_id"]').selectOption(providers[0].id);
    await second.locator('[name="upstream_model"]').fill("duplicate");
    await page.locator("#routing-form button[type=submit]").click();
    await page.getByText("A route already exists for this model name and provider.").waitFor();
    assert.equal(await page.locator(".route-row").count(), 2);
    const routing = await (await context.request.get(`${apiUrl}/management/routing`)).json();
    assert.equal(routing.routes.length, 1);
    await second.locator('[name="provider_id"]').selectOption(providers[1].id);
    await page.locator("#routing-form button[type=submit]").click();
    await page.getByText("Model routes saved.").waitFor();

    // A slow bootstrap read must not overwrite edits made after a newer refresh.
    let releaseRouting;
    const routingGate = new Promise((resolve) => { releaseRouting = resolve; });
    let routingEntered;
    const oldRouting = new Promise((resolve) => { routingEntered = resolve; });
    let routingReads = 0;
    await page.route(`${apiUrl}/management/routing`, async (route) => {
      routingReads += 1;
      if (routingReads === 1) {
        const response = await route.fetch();
        routingEntered();
        await routingGate;
        await route.fulfill({ response });
      } else await route.continue();
    });
    await page.reload();
    await oldRouting;
    await page.locator('nav [data-view="routing"]').click();
    await page.locator("#refresh-button").click();
    await page.waitForFunction(() => !document.getElementById("routing-fields").disabled);
    await page.locator('.route-row [name="model"]').first().fill("unsaved-route");
    const staleResponse = page.waitForResponse(`${apiUrl}/management/routing`);
    releaseRouting();
    await staleResponse;
    await page.waitForFunction(() => document.getElementById("updated-at").textContent.includes("Updated at"));
    assert.equal(await page.locator('.route-row [name="model"]').first().inputValue(), "unsaved-route");
    await page.unroute(`${apiUrl}/management/routing`);

    await page.locator('nav [data-view="account"]').click();
    await page.locator("#account-label").fill("Production workspace");
    let releaseAccount;
    const accountGate = new Promise((resolve) => { releaseAccount = resolve; });
    let accountEntered;
    const pendingAccount = new Promise((resolve) => { accountEntered = resolve; });
    await page.route(`${apiUrl}/management/account`, async (route) => {
      accountEntered();
      await accountGate;
      await route.abort().catch(() => {});
    });
    await page.locator("#account-form button[type=submit]").click();
    await pendingAccount;
    assert.equal(await page.locator("#account-label").isEnabled(), false);
    await page.route(`${apiUrl}/management/session`, (route) => route.fulfill({
      status: 503, contentType: "application/json", body: JSON.stringify({ error: { message: "Temporary failure" } }),
    }));
    await page.locator("#logout-button").click();
    await page.getByText("Could not sign out. Temporary failure", { exact: true }).waitFor();
    assert.equal(await page.locator("#account-label").inputValue(), "Production workspace");
    assert.equal(await page.locator("#account-label").isEnabled(), true);
    releaseAccount();
    await page.unroute(`${apiUrl}/management/account`);
    await page.unroute(`${apiUrl}/management/session`);
    await page.locator("#account-form button[type=submit]").click();
    await page.getByText("Account name saved.").waitFor();
    assert.equal(await page.locator("#sidebar-label").textContent(), "Production workspace");
    await page.reload();
    await page.locator("#app").waitFor({ state: "visible" });
    await page.waitForFunction(() => document.getElementById("summary-requests").textContent === "0");
    if (screenshots) await page.screenshot({ path: `${screenshots}/overview.png`, fullPage: true });
    await page.setViewportSize({ width: 390, height: 844 });
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
    if (screenshots) await page.screenshot({ path: `${screenshots}/mobile.png`, fullPage: true });
    for (const view of ["tokens", "providers", "routing", "account"]) {
      await page.locator(`nav [data-view="${view}"]`).click();
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `${view} fits on mobile`);
      if (screenshots) await page.screenshot({ path: `${screenshots}/mobile-${view}.png`, fullPage: true });
    }
    await page.setViewportSize({ width: 1440, height: 1000 });

    await page.locator('nav [data-view="tokens"]').click();
    await page.getByRole("button", { name: "Revoke Production app token", exact: true }).click();
    await page.getByText("Token revoked. Existing usage history is retained.").waitFor();
    const tokenList = await (await context.request.get(`${apiUrl}/management/api-tokens`)).json();
    assert.ok(tokenList.find((token) => token.id === tokens[1].id).revoked_at);
    let release;
    const gate = new Promise((resolve) => { release = resolve; });
    let entered;
    const intercepted = new Promise((resolve) => { entered = resolve; });
    await page.route(`${apiUrl}/management/api-tokens`, async (route) => {
      entered();
      await gate;
      await route.continue().catch(() => {});
    });
    await page.locator("#refresh-button").click();
    await intercepted;
    await page.locator("#logout-button").click();
    await page.locator("#login-form").waitFor({ state: "visible" });
    release();
    await page.unroute(`${apiUrl}/management/api-tokens`);
    await page.locator("#login-email").fill(email);
    await page.locator("#login-password").fill(password);
    await page.locator("#login-form button[type=submit]").click();
    await page.locator("#app").waitFor({ state: "visible" });
    await page.waitForFunction(() => !document.getElementById("refresh-button").disabled);
    await page.locator("#logout-button").click();
    await page.locator("#login-form").waitFor({ state: "visible" });
    assert.equal(await page.locator("#token-list").textContent(), "");
    assert.equal(await page.locator("#issued-token").inputValue(), "");
    assert.equal(await page.evaluate(() => localStorage.length + sessionStorage.length), 0);
    assert.deepEqual(errors, []);
    console.log("Dashboard browser checks passed: cross-origin session, Unicode password, tokens, providers, OAuth fixtures, routing, mobile layout, and logout races.");
  } finally {
    await browser.close();
  }
})().catch((error) => { console.error(error); process.exitCode = 1; });
