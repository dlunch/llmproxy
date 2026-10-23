"use strict";

const numbers = new Intl.NumberFormat("ko-KR");
const dates = new Intl.DateTimeFormat("ko-KR", { dateStyle: "medium", timeStyle: "short" });
const viewNames = { overview: "개요", tokens: "API 토큰", providers: "제공자", routing: "모델 경로", account: "계정" };
const state = {
  apiUrl: "", epoch: 0, controller: new AbortController(), account: null,
  view: "overview", tokens: null, providers: null, routing: null,
  metrics: new Map(), metricsVersion: 0, tokenVersion: 0, providerVersion: 0, routingVersion: 0,
  routingDirty: false, routeSequence: 0, oauth: null, authBusy: false,
};

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function showMessage(id, text = "", error = false) {
  const node = document.getElementById(id);
  node.textContent = text;
  node.hidden = !text;
  node.classList.toggle("error", error);
}

function date(value) {
  return value ? dates.format(new Date(value)) : "기록 없음";
}

function empty(container, title, description) {
  const node = element("div", "empty-state");
  node.append(element("strong", "", title));
  if (description) node.append(element("p", "", description));
  container.replaceChildren(node);
}

function explain(error) {
  const messages = {
    "Invalid email or password": "이메일 또는 비밀번호를 확인해 주세요.",
    "Invalid signup code": "가입 코드가 올바르지 않거나 이미 사용되었습니다.",
    "Invalid email address": "이메일 주소를 확인해 주세요.",
    "Password must contain 8 to 1024 bytes": "비밀번호는 UTF-8 기준 8~1024바이트로 입력해 주세요.",
    "Resource already exists": "이미 사용 중인 값입니다. 입력 내용을 확인해 주세요.",
    "Password authentication is busy": "로그인 요청이 많습니다. 잠시 후 다시 시도해 주세요.",
    "Label must contain 1 to 256 bytes": "이름을 입력해 주세요. 한글은 85자 이내로 입력할 수 있습니다.",
    "Submit the full code from this provider login": "이 로그인에서 받은 코드와 # 뒤의 문자열을 모두 입력해 주세요.",
    "Provider login is no longer active": "로그인이 만료되었거나 종료되었습니다. 새 로그인을 시작해 주세요.",
    "Provider login not found": "로그인이 만료되었거나 새 로그인으로 바뀌었습니다.",
    "Provider not found": "제공자를 찾을 수 없습니다. 제공자 목록을 새로 고침해 주세요.",
    "Duplicate model/provider mapping": "같은 모델 이름과 제공자를 연결한 경로가 중복되어 있습니다.",
    "A provider login is already processing": "이 제공자의 로그인이 처리 중입니다. 잠시 후 다시 시도해 주세요.",
  };
  return messages[error.message] || error.message || "요청을 처리하지 못했습니다. 다시 시도해 주세요.";
}

async function api(path, { method = "GET", body, signal, authenticated = true } = {}) {
  const epoch = state.epoch;
  const headers = { Accept: "application/json" };
  if (method !== "GET") headers["X-LLMProxy-Management"] = "1";
  if (body !== undefined) headers["Content-Type"] = "application/json";
  let response;
  try {
    response = await fetch(`${state.apiUrl}/management${path}`, {
      method, headers, credentials: "include", cache: "no-store",
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: signal || state.controller.signal,
    });
  } catch (error) {
    if (error.name === "AbortError" || epoch !== state.epoch) throw new DOMException("Stale request", "AbortError");
    throw new Error("API에 연결하지 못했습니다. 네트워크 연결을 확인한 후 다시 시도해 주세요.");
  }
  const text = response.status === 204 ? "" : await response.text();
  if (epoch !== state.epoch) throw new DOMException("Stale request", "AbortError");
  let data = null;
  if (text) {
    try { data = JSON.parse(text); }
    catch { throw new Error("API 응답을 읽지 못했습니다. 잠시 후 다시 시도해 주세요."); }
  }
  if (!response.ok) {
    if (response.status === 401 && authenticated && state.account) {
      showAuth("로그인 시간이 만료되었습니다. 다시 로그인해 주세요.");
      throw new DOMException("Session ended", "AbortError");
    }
    const error = new Error(data?.error?.message || `요청에 실패했습니다. (HTTP ${response.status})`);
    error.status = response.status;
    throw error;
  }
  return data;
}

function clearToken() {
  document.getElementById("issued-token").value = "";
  showMessage("copy-message");
}

function stopOAuth() {
  if (state.oauth) {
    clearTimeout(state.oauth.timer);
    state.oauth.controller.abort();
    state.oauth = null;
  }
  document.getElementById("oauth-code").value = "";
  document.getElementById("device-code").textContent = "";
  document.getElementById("oauth-link").removeAttribute("href");
}

function invalidateRequests() {
  state.epoch += 1;
  state.controller.abort();
  state.controller = new AbortController();
  document.getElementById("refresh-button").disabled = false;
  for (const form of document.forms) {
    delete form.dataset.busy;
    form.pendingOperation = null;
    for (const control of form.elements) control.disabled = false;
  }
  stopOAuth();
  for (const id of ["oauth-dialog", "token-dialog"]) {
    if (document.getElementById(id).open) document.getElementById(id).close();
  }
  clearToken();
}

function showAuth(message = "") {
  invalidateRequests();
  state.account = null;
  state.tokens = state.providers = state.routing = null;
  state.metrics.clear();
  state.routingDirty = false;
  state.authBusy = false;
  document.getElementById("app").hidden = true;
  document.getElementById("boot").hidden = true;
  document.getElementById("auth-screen").hidden = false;
  for (const form of document.forms) {
    form.reset();
    delete form.dataset.busy;
    for (const control of form.elements) control.disabled = false;
  }
  for (const id of ["token-list", "provider-list", "route-list", "overview-tokens", "metrics-content", "metrics-token"]) document.getElementById(id).replaceChildren();
  for (const id of ["sidebar-label", "sidebar-email", "account-avatar", "account-created"]) document.getElementById(id).textContent = "";
  document.getElementById("account-email").value = "";
  document.getElementById("token-metrics").hidden = true;
  showMessage("auth-message", message);
  selectAuth("login");
}

function selectAuth(mode) {
  const login = mode === "login";
  document.getElementById("login-form").hidden = !login;
  document.getElementById("signup-form").hidden = login;
  for (const [id, active] of [["login-tab", login], ["signup-tab", !login]]) {
    document.getElementById(id).classList.toggle("active", active);
    document.getElementById(id).setAttribute("aria-pressed", String(active));
  }
}

function renderAccount() {
  const account = state.account;
  document.getElementById("sidebar-label").textContent = account.label;
  document.getElementById("sidebar-email").textContent = account.email;
  document.getElementById("account-avatar").textContent = Array.from(account.label)[0]?.toUpperCase() || "L";
  document.getElementById("account-label").value = account.label;
  document.getElementById("account-email").value = account.email;
  document.getElementById("account-created").textContent = date(account.created_at);
}

function showView(view, focus = false) {
  if (!Object.hasOwn(viewNames, view)) return;
  state.view = view;
  for (const name of Object.keys(viewNames)) document.getElementById(`view-${name}`).hidden = name !== view;
  for (const button of document.querySelectorAll("nav [data-view]")) {
    const active = button.dataset.view === view;
    button.classList.toggle("active", active);
    if (active) button.setAttribute("aria-current", "page");
    else button.removeAttribute("aria-current");
  }
  document.getElementById("current-view").textContent = viewNames[view];
  document.title = `${viewNames[view]} · llmproxy`;
  if (focus) document.getElementById("main-content").focus();
}

async function enterAccount(account) {
  invalidateRequests();
  state.account = account;
  state.tokens = state.providers = state.routing = null;
  state.metrics.clear();
  state.routingDirty = false;
  document.getElementById("auth-screen").hidden = true;
  document.getElementById("boot").hidden = true;
  document.getElementById("app").hidden = false;
  document.getElementById("login-password").value = "";
  document.getElementById("signup-password").value = "";
  document.getElementById("signup-code").value = "";
  for (const id of ["global-message", "token-message", "provider-message", "routing-message", "account-message"]) showMessage(id);
  renderAccount();
  showView("overview");
  renderOverview();
  await loadAll();
}

async function submit(form, messageId, work) {
  if (form.dataset.busy) return;
  const data = new FormData(form);
  const epoch = state.epoch;
  const operation = Symbol();
  form.pendingOperation = operation;
  form.dataset.busy = "true";
  for (const control of form.elements) control.disabled = true;
  showMessage(messageId);
  try { await work(data); }
  catch (error) {
    if (error.name !== "AbortError" && epoch === state.epoch && form.pendingOperation === operation) showMessage(messageId, explain(error), true);
  } finally {
    if (epoch === state.epoch && form.pendingOperation === operation) {
      delete form.dataset.busy;
      for (const control of form.elements) control.disabled = false;
    }
  }
}

async function loadAll() {
  const epoch = state.epoch;
  const results = await Promise.allSettled([loadTokens(), loadProviders(), loadRouting()]);
  if (epoch !== state.epoch) return;
  const failure = results.find((result) => result.status === "rejected" && result.reason.name !== "AbortError");
  if (failure) showMessage("global-message", explain(failure.reason), true);
  document.getElementById("updated-at").textContent = `${new Intl.DateTimeFormat("ko-KR", { hour: "numeric", minute: "2-digit" }).format(new Date())} 기준`;
}

async function loadTokens() {
  const version = ++state.tokenVersion;
  if (!state.tokens) empty(document.getElementById("token-list"), "토큰을 불러오고 있습니다.");
  const tokens = await api("/api-tokens");
  if (version !== state.tokenVersion) return;
  state.tokens = tokens;
  renderTokens();
  renderOverview();
  await loadMetrics();
}

async function loadMetrics() {
  const version = ++state.metricsVersion;
  const epoch = state.epoch;
  const tokens = state.tokens || [];
  state.metrics = new Map(tokens.map((token) => [token.id, { loading: true }]));
  renderTokens();
  renderOverview();
  await Promise.all(tokens.map(async (token) => {
    try {
      const value = await api(`/api-tokens/${encodeURIComponent(token.id)}/metrics`);
      if (version !== state.metricsVersion || epoch !== state.epoch) return;
      state.metrics.set(token.id, { value });
    } catch (error) {
      if (error.name === "AbortError" || version !== state.metricsVersion || epoch !== state.epoch) return;
      state.metrics.set(token.id, { error: explain(error) });
    }
  }));
  if (version === state.metricsVersion && epoch === state.epoch) {
    renderTokens();
    renderOverview();
  }
}

async function loadProviders() {
  const version = ++state.providerVersion;
  if (!state.providers) empty(document.getElementById("provider-list"), "제공자를 불러오고 있습니다.");
  const providers = await api("/providers");
  if (version !== state.providerVersion) return;
  state.providers = providers;
  renderProviders();
  updateRouteProviders();
  renderOverview();
}

async function loadRouting() {
  const version = ++state.routingVersion;
  document.getElementById("routing-fields").disabled = true;
  const epoch = state.epoch;
  try {
    const routing = await api("/routing");
    if (version !== state.routingVersion || epoch !== state.epoch) return;
    state.routing = routing;
    state.routingDirty = false;
    renderRouting();
    renderOverview();
  } finally {
    if (version === state.routingVersion && epoch === state.epoch) document.getElementById("routing-fields").disabled = !state.routing;
  }
}

function renderOverview() {
  const tokens = state.tokens || [];
  const values = tokens.map((token) => state.metrics.get(token.id)?.value);
  const complete = state.tokens !== null && values.every(Boolean);
  for (const [id, field] of [["summary-requests", "request_count"], ["summary-input", "input_tokens"], ["summary-output", "output_tokens"]]) {
    document.getElementById(id).textContent = complete ? numbers.format(values.reduce((sum, value) => sum + value[field], 0)) : "—";
  }
  document.getElementById("summary-providers").textContent = state.providers ? numbers.format(state.providers.filter((provider) => provider.authenticated).length) : "—";
  document.getElementById("setup-providers").textContent = state.providers?.length ? `${state.providers.length}개 제공자 중 ${state.providers.filter((provider) => provider.authenticated && provider.enabled).length}개에 인증 정보가 저장되어 있고 활성화되어 있습니다.` : "제공자를 등록하고 로그인하세요.";
  document.getElementById("setup-routes").textContent = state.routing?.routes.length ? `${state.routing.routes.length}개 경로가 저장되어 있습니다.` : "요청할 모델 이름과 제공자를 연결하세요.";
  const active = tokens.filter((token) => !token.revoked_at).length;
  document.getElementById("setup-tokens").textContent = active ? `${active}개 API 토큰을 사용할 수 있습니다.` : "앱에서 사용할 토큰을 만드세요.";
  const list = document.getElementById("overview-tokens");
  list.replaceChildren();
  if (!state.tokens) { empty(list, "사용량을 불러오고 있습니다."); return; }
  if (!tokens.length) { empty(list, "아직 발급한 토큰이 없습니다.", "첫 API 토큰을 만들면 이곳에서 사용량을 확인할 수 있습니다."); return; }
  for (const token of [...tokens].reverse().slice(0, 5)) {
    const metric = state.metrics.get(token.id);
    const row = element("div", "compact-row");
    const name = element("div");
    name.append(element("strong", "", token.label), element("small", "", token.revoked_at ? "폐기됨" : "활성 토큰"));
    row.append(name, element("span", "", metric?.value ? `${numbers.format(metric.value.request_count)}건` : metric?.error ? "조회 실패" : "불러오는 중"));
    list.append(row);
  }
}

function renderTokens() {
  if (!state.tokens) return;
  const list = document.getElementById("token-list");
  const selected = document.getElementById("metrics-token").value;
  const select = document.getElementById("metrics-token");
  select.replaceChildren();
  document.getElementById("token-count").textContent = `전체 ${state.tokens.length}개`;
  document.getElementById("token-metrics").hidden = !state.tokens.length;
  if (!state.tokens.length) {
    empty(list, "앱에 사용할 첫 토큰을 만들어 보세요.", "토큰을 발급한 후에는 이름과 사용 기록만 표시됩니다.");
    return;
  }
  const table = element("table");
  const head = element("thead");
  const header = element("tr");
  for (const title of ["이름 / 상태", "발급일", "요청", "입력 / 출력 토큰", "관리"]) header.append(element("th", "", title));
  head.append(header);
  const body = element("tbody");
  for (const token of [...state.tokens].reverse()) {
    select.append(new Option(`${token.label}${token.revoked_at ? " · 폐기됨" : ""}`, token.id));
    const metric = state.metrics.get(token.id);
    const row = element("tr");
    const name = element("td");
    const content = element("div", "token-name");
    const button = element("button", "", token.label);
    button.type = "button";
    button.addEventListener("click", () => {
      select.value = token.id;
      renderTokenMetrics();
      document.getElementById("metrics-title").focus();
    });
    content.append(button, element("span", `badge ${token.revoked_at ? "neutral" : "success"}`, token.revoked_at ? "폐기됨" : "활성"));
    name.append(content);
    const count = metric?.value ? numbers.format(metric.value.request_count) : metric?.error ? "조회 실패" : "…";
    const usage = metric?.value ? `${numbers.format(metric.value.input_tokens)} / ${numbers.format(metric.value.output_tokens)}` : "—";
    row.append(name, element("td", "muted", date(token.created_at)), element("td", "table-number", count), element("td", "table-number", usage));
    const action = element("td");
    if (!token.revoked_at) {
      const revoke = element("button", "danger-button", "폐기");
      revoke.type = "button";
      revoke.setAttribute("aria-label", `${token.label} 토큰 폐기`);
      revoke.addEventListener("click", async () => {
        if (!window.confirm(`‘${token.label}’ 토큰을 폐기할까요? 이 토큰으로는 새 요청을 보낼 수 없습니다.`)) return;
        revoke.disabled = true;
        try {
          await api(`/api-tokens/${encodeURIComponent(token.id)}`, { method: "DELETE" });
          state.tokenVersion += 1;
          state.tokens = state.tokens.map((item) => item.id === token.id ? { ...item, revoked_at: new Date().toISOString() } : item);
          renderTokens();
          renderOverview();
          showMessage("token-message", "토큰을 폐기했습니다. 기존 사용 기록은 유지됩니다.");
        } catch (error) {
          if (error.name !== "AbortError") { showMessage("token-message", explain(error), true); revoke.disabled = false; }
        }
      });
      action.append(revoke);
    } else action.append(element("span", "muted", "—"));
    row.append(action);
    body.append(row);
  }
  table.append(head, body);
  const scroll = element("div", "table-scroll");
  scroll.append(table);
  list.replaceChildren(scroll);
  if (state.tokens.some((token) => token.id === selected)) select.value = selected;
  renderTokenMetrics();
}

function renderTokenMetrics() {
  const container = document.getElementById("metrics-content");
  const token = state.tokens?.find((item) => item.id === document.getElementById("metrics-token").value);
  if (!token) return;
  const result = state.metrics.get(token.id);
  document.getElementById("metrics-title").textContent = `${token.label} 사용량`;
  if (!result?.value) {
    empty(container, result?.error || "사용량을 불러오고 있습니다.", result?.error ? "새로 고침을 누르면 다시 조회합니다." : "");
    return;
  }
  const value = result.value;
  const grid = element("dl", "metrics-grid");
  for (const [label, field] of [["전체 요청", "request_count"], ["성공", "succeeded_requests"], ["실행 중", "running_requests"], ["입력 토큰", "input_tokens"], ["출력 토큰", "output_tokens"], ["평균 처리 시간", "mean_execution_latency_ms"]]) {
    const item = element("div", "metric-item");
    const formatted = field === "mean_execution_latency_ms" ? value[field] === null ? "—" : `${numbers.format(Math.round(value[field]))} ms` : numbers.format(value[field]);
    item.append(element("dt", "", label), element("dd", "", formatted));
    grid.append(item);
  }
  const details = element("div", "metric-details");
  for (const [label, field] of [["실패", "failed_requests"], ["취소", "cancelled_requests"], ["시간 초과", "timed_out_requests"], ["중단", "interrupted_requests"], ["사용량 미수신", "missing_usage_requests"]]) details.append(element("span", "", `${label} ${numbers.format(value[field])}건`));
  details.append(element("span", "", `마지막 요청: ${date(value.last_request_at)}`));
  container.replaceChildren(grid, details);
}

function renderProviders() {
  if (!state.providers) return;
  const list = document.getElementById("provider-list");
  if (!state.providers.length) { empty(list, "연결된 제공자가 없습니다.", "Codex 또는 Claude를 등록한 뒤 로그인해 주세요."); return; }
  const existing = new Map([...list.querySelectorAll(".provider-card")].map((card) => [card.dataset.providerId, card]));
  const cards = [];
  for (const provider of state.providers) {
    let card = existing.get(provider.id);
    if (!card) {
      card = element("article", "panel provider-card");
      card.dataset.providerId = provider.id;
      const heading = element("div", "panel-heading");
      const kind = element("div", "provider-kind");
      kind.append(element("span", "provider-icon", provider.kind === "codex" ? "O" : "C"), element("h2", "", provider.kind === "codex" ? "Codex" : "Claude"));
      heading.append(kind);
      card.append(heading, element("span", "badge provider-state"));
      const form = element("form");
      const label = element("label", "", "제공자 이름");
      const input = element("input");
      input.id = `provider-label-${provider.id}`;
      input.name = "label";
      input.maxLength = 256;
      input.required = true;
      label.htmlFor = input.id;
      const enabledLabel = element("label", "checkbox-label");
      const enabled = element("input");
      enabled.type = "checkbox";
      enabled.name = "enabled";
      enabledLabel.append(enabled, document.createTextNode("모델 요청에 사용"));
      const actions = element("div", "provider-actions");
      const login = element("button", "secondary", "로그인");
      login.type = "button";
      login.dataset.action = "login";
      login.addEventListener("click", () => openOAuth(state.providers.find((item) => item.id === provider.id)));
      const save = element("button", "primary", "설정 저장");
      save.type = "submit";
      actions.append(login, save);
      const message = element("p", "form-message");
      message.id = `provider-message-${provider.id}`;
      message.setAttribute("role", "status");
      message.hidden = true;
      form.append(label, input, enabledLabel, actions, message);
      form.addEventListener("submit", (event) => {
        event.preventDefault();
        submit(form, message.id, async (data) => {
          const updated = await api(`/providers/${encodeURIComponent(provider.id)}`, { method: "PATCH", body: { label: data.get("label").trim(), enabled: data.get("enabled") === "on" } });
          state.providerVersion += 1;
          state.providers = state.providers.map((item) => item.id === updated.id ? updated : item);
          input.value = updated.label;
          input.dataset.saved = updated.label;
          enabled.checked = updated.enabled;
          enabled.dataset.saved = String(updated.enabled);
          renderProviders();
          updateRouteProviders();
          renderOverview();
          showMessage(message.id, "제공자 설정을 저장했습니다.");
        });
      });
      card.append(form);
    }
    const input = card.querySelector("input[name=label]");
    const enabled = card.querySelector("input[name=enabled]");
    if (input.dataset.saved === undefined || input.value === input.dataset.saved) input.value = provider.label;
    input.dataset.saved = provider.label;
    if (enabled.dataset.saved === undefined || String(enabled.checked) === enabled.dataset.saved) enabled.checked = provider.enabled;
    enabled.dataset.saved = String(provider.enabled);
    const badge = card.querySelector(".provider-state");
    badge.textContent = provider.authenticated ? "인증 정보 저장됨" : "로그인 필요";
    badge.className = `badge provider-state ${provider.authenticated ? "success" : "warning"}`;
    card.querySelector("[data-action=login]").textContent = provider.authenticated ? "다시 로그인" : "로그인";
    cards.push(card);
  }
  list.replaceChildren(...cards);
}

function markRoutingDirty() {
  state.routingDirty = true;
  document.getElementById("routing-dirty").hidden = false;
  showMessage("routing-message");
}

function updateRouteProviders() {
  for (const select of document.getElementById("route-list").querySelectorAll("select[name=provider_id]")) {
    const selected = select.value || select.dataset.initial || "";
    select.replaceChildren(new Option("제공자 선택", ""));
    for (const provider of state.providers || []) select.append(new Option(`${provider.label}${provider.enabled ? "" : " · 비활성"}`, provider.id));
    if (selected && !state.providers?.some((provider) => provider.id === selected)) select.append(new Option("사용할 수 없는 제공자", selected));
    select.value = selected;
    delete select.dataset.initial;
  }
}

function addRoute(route = { model: "", provider_id: "", upstream_model: "", priority: 0 }) {
  if (!document.getElementById("route-list").querySelector(".route-row")) document.getElementById("route-list").replaceChildren();
  const row = element("div", "route-row");
  const sequence = ++state.routeSequence;
  for (const [name, title, placeholder] of [["model", "요청할 모델 이름", "예: my-model"], ["provider_id", "제공자", ""], ["upstream_model", "제공자의 모델 이름", "예: claude-sonnet-4-6"], ["priority", "우선순위", "0"]]) {
    const field = element("div");
    const label = element("label", "", title);
    const input = element(name === "provider_id" ? "select" : "input");
    input.name = name;
    input.id = `route-${sequence}-${name}`;
    input.required = true;
    label.htmlFor = input.id;
    if (name === "provider_id") input.dataset.initial = route[name];
    else {
      input.value = route[name];
      input.placeholder = placeholder;
      if (name === "priority") { input.type = "number"; input.min = "-2147483648"; input.max = "2147483647"; input.step = "1"; }
      else input.maxLength = 256;
    }
    field.append(label, input);
    row.append(field);
  }
  const remove = element("button", "route-remove", "삭제");
  remove.type = "button";
  remove.setAttribute("aria-label", "이 모델 경로 삭제");
  remove.addEventListener("click", () => {
    row.remove();
    markRoutingDirty();
    if (!document.getElementById("route-list").children.length) empty(document.getElementById("route-list"), "저장할 모델 경로가 없습니다.", "경로를 추가하거나 저장하여 기존 경로를 모두 비울 수 있습니다.");
    document.getElementById("add-route").focus();
  });
  row.append(remove);
  document.getElementById("route-list").append(row);
  updateRouteProviders();
  return row;
}

function renderRouting() {
  document.getElementById("routing-strategy").value = state.routing.strategy;
  document.getElementById("route-list").replaceChildren();
  for (const route of state.routing.routes) addRoute(route);
  if (!state.routing.routes.length) empty(document.getElementById("route-list"), "아직 설정한 모델 경로가 없습니다.", "제공자를 등록한 뒤 첫 경로를 추가하세요.");
  document.getElementById("routing-dirty").hidden = !state.routingDirty;
}

async function openOAuth(provider) {
  stopOAuth();
  const flow = { provider, login: null, timer: null, controller: new AbortController() };
  state.oauth = flow;
  document.getElementById("oauth-title").textContent = `${provider.label} 로그인`;
  document.getElementById("oauth-description").textContent = "로그인을 준비하고 있습니다.";
  for (const id of ["oauth-link-section", "device-section", "oauth-code-form", "oauth-retry", "oauth-restart"]) document.getElementById(id).hidden = true;
  document.getElementById("oauth-code").value = "";
  for (const control of document.getElementById("oauth-code-form").elements) control.disabled = false;
  delete document.getElementById("oauth-code-form").dataset.busy;
  document.getElementById("oauth-code-form").pendingOperation = null;
  showMessage("oauth-message");
  if (!document.getElementById("oauth-dialog").open) document.getElementById("oauth-dialog").showModal();
  try {
    const login = await api(`/providers/${encodeURIComponent(provider.id)}/logins`, { method: "POST", signal: flow.controller.signal });
    if (state.oauth !== flow) return;
    flow.login = login;
    const url = new URL(login.authorization_url);
    if (!["https:", "http:"].includes(url.protocol)) throw new Error("제공자 로그인 주소가 올바르지 않습니다.");
    document.getElementById("oauth-link").href = url.href;
    document.getElementById("oauth-link-section").hidden = false;
    document.getElementById("oauth-expiry").textContent = `로그인 유효 시간: ${date(login.expires_at)}`;
    if (provider.kind === "codex") {
      document.getElementById("oauth-description").textContent = "로그인 페이지를 열어 기기 코드를 입력하면 연결이 자동으로 완료됩니다.";
      document.getElementById("device-code").textContent = login.user_code;
      document.getElementById("device-section").hidden = false;
      scheduleOAuth(flow);
    } else {
      document.getElementById("oauth-description").textContent = "로그인 페이지에서 인증한 뒤, 표시된 전체 코드를 아래에 붙여 넣으세요.";
      document.getElementById("oauth-code-form").hidden = false;
    }
  } catch (error) {
    if (error.name !== "AbortError" && state.oauth === flow) {
      showMessage("oauth-message", explain(error), true);
      document.getElementById("oauth-restart").hidden = false;
    }
  }
}

function scheduleOAuth(flow) {
  clearTimeout(flow.timer);
  if (state.oauth !== flow) return;
  const next = flow.provider.kind === "codex" ? Math.max(1000, (flow.login.interval_seconds || 1) * 1000, Date.parse(flow.login.next_poll_at || "") - Date.now() || 0) : 2000;
  flow.timer = setTimeout(() => pollOAuth(flow), next);
}

async function showOAuthStatus(flow, login) {
  if (state.oauth !== flow) return;
  flow.login = login;
  document.getElementById("oauth-retry").hidden = true;
  if (login.status === "completed") {
    clearTimeout(flow.timer);
    document.getElementById("oauth-link-section").hidden = true;
    document.getElementById("oauth-code-form").hidden = true;
    document.getElementById("device-section").hidden = true;
    document.getElementById("oauth-code").value = "";
    showMessage("oauth-message", "로그인을 완료하고 인증 정보를 저장했습니다.");
    const provider = await api(`/providers/${encodeURIComponent(flow.provider.id)}`, { signal: flow.controller.signal });
    if (state.oauth !== flow) return;
    state.providerVersion += 1;
    state.providers = state.providers.map((item) => item.id === provider.id ? provider : item);
    renderProviders();
    renderOverview();
  } else if (["failed", "expired"].includes(login.status)) {
    clearTimeout(flow.timer);
    showMessage("oauth-message", login.status === "expired" ? "로그인 시간이 만료되었습니다. 새 로그인을 시작해 주세요." : "로그인을 완료하지 못했습니다. 새 로그인을 시작해 주세요.", true);
    document.getElementById("oauth-code-form").hidden = true;
    document.getElementById("oauth-link-section").hidden = true;
    document.getElementById("oauth-restart").hidden = false;
  } else {
    document.getElementById("device-status").textContent = login.status === "processing" ? "인증 정보를 저장하고 있습니다." : "인증 완료를 기다리고 있습니다.";
    if (flow.provider.kind === "claude") showMessage("oauth-message", "인증 정보를 확인하고 있습니다.");
    scheduleOAuth(flow);
  }
}

async function pollOAuth(flow) {
  if (state.oauth !== flow || !flow.login) return;
  document.getElementById("oauth-retry").hidden = true;
  const path = `/providers/${encodeURIComponent(flow.provider.id)}/logins/${encodeURIComponent(flow.login.id)}`;
  try {
    const login = await api(flow.provider.kind === "codex" ? `${path}/complete` : path, {
      method: flow.provider.kind === "codex" ? "POST" : "GET",
      body: flow.provider.kind === "codex" ? {} : undefined,
      signal: flow.controller.signal,
    });
    await showOAuthStatus(flow, login);
  } catch (error) {
    if (error.name !== "AbortError" && state.oauth === flow) {
      showMessage("oauth-message", explain(error), true);
      if (error.status === 404 || error.status === 409) document.getElementById("oauth-restart").hidden = false;
      else document.getElementById("oauth-retry").hidden = false;
    }
  }
}

for (const button of document.querySelectorAll("[data-view]")) button.addEventListener("click", () => showView(button.dataset.view, true));
for (const button of document.querySelectorAll("[data-close]")) button.addEventListener("click", () => document.getElementById(button.dataset.close).close());
document.getElementById("token-dialog").addEventListener("close", clearToken);
document.getElementById("oauth-dialog").addEventListener("close", stopOAuth);
document.getElementById("login-tab").addEventListener("click", () => selectAuth("login"));
document.getElementById("signup-tab").addEventListener("click", () => selectAuth("signup"));
document.getElementById("metrics-token").addEventListener("change", renderTokenMetrics);

for (const [id, signup] of [["login-form", false], ["signup-form", true]]) {
  const form = document.getElementById(id);
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    if (state.authBusy || !state.apiUrl) return;
    state.authBusy = true;
    const epoch = state.epoch;
    await submit(form, "auth-message", async (data) => {
      const credentials = { email: data.get("email"), password: data.get("password") };
      if (signup) {
        await api("/accounts", { method: "POST", authenticated: false, body: { ...credentials, label: data.get("label").trim(), signup_code: data.get("signup_code").trim() } });
        document.getElementById("signup-code").value = "";
        document.getElementById("login-email").value = credentials.email;
        showMessage("auth-message", "계정을 만들었습니다. 로그인하고 있습니다.");
      }
      try {
        const result = await api("/sessions", { method: "POST", authenticated: false, body: credentials });
        await enterAccount(result.account);
      } catch (error) {
        if (signup && error.name !== "AbortError") {
          selectAuth("login");
          throw new Error(`계정은 생성되었습니다. 로그인해 주세요. ${explain(error)}`);
        }
        throw error;
      }
    });
    if (epoch === state.epoch) state.authBusy = false;
  });
}

document.getElementById("token-form").addEventListener("submit", (event) => {
  event.preventDefault();
  submit(event.currentTarget, "token-message", async (data) => {
    if (!state.tokens) throw new Error("토큰 목록을 먼저 불러와 주세요.");
    const issued = await api("/api-tokens", { method: "POST", body: { label: data.get("label").trim() } });
    state.tokenVersion += 1;
    const { token, ...metadata } = issued;
    state.tokens.push(metadata);
    document.getElementById("token-label").value = "";
    clearToken();
    document.getElementById("issued-token").value = token;
    document.getElementById("token-dialog").showModal();
    renderTokens();
    await loadMetrics();
  });
});

document.getElementById("copy-token").addEventListener("click", async () => {
  const epoch = state.epoch;
  try {
    await navigator.clipboard.writeText(document.getElementById("issued-token").value);
    if (epoch === state.epoch && document.getElementById("token-dialog").open) showMessage("copy-message", "토큰을 복사했습니다.");
  } catch {
    if (epoch === state.epoch && document.getElementById("token-dialog").open) {
      document.getElementById("issued-token").select();
      showMessage("copy-message", "복사 권한을 사용할 수 없습니다. 선택된 토큰을 직접 복사해 주세요.", true);
    }
  }
});

document.getElementById("provider-form").addEventListener("submit", (event) => {
  event.preventDefault();
  submit(event.currentTarget, "provider-message", async (data) => {
    if (!state.providers) throw new Error("제공자 목록을 먼저 불러와 주세요.");
    const provider = await api("/providers", { method: "POST", body: { kind: data.get("kind"), label: data.get("label").trim() } });
    state.providerVersion += 1;
    state.providers.push(provider);
    document.getElementById("provider-label").value = "";
    renderProviders();
    updateRouteProviders();
    renderOverview();
    showMessage("provider-message", "제공자를 등록했습니다. 로그인하여 구독 계정을 연결하세요.");
  });
});

document.getElementById("routing-form").addEventListener("input", markRoutingDirty);
document.getElementById("routing-form").addEventListener("change", markRoutingDirty);
document.getElementById("add-route").addEventListener("click", () => {
  const row = addRoute();
  markRoutingDirty();
  row.querySelector("input").focus();
});
document.getElementById("routing-form").addEventListener("submit", (event) => {
  event.preventDefault();
  const routes = [...document.getElementById("route-list").querySelectorAll(".route-row")].map((row) => ({
    model: row.querySelector("[name=model]").value.trim(),
    provider_id: row.querySelector("[name=provider_id]").value,
    upstream_model: row.querySelector("[name=upstream_model]").value.trim(),
    priority: Number(row.querySelector("[name=priority]").value),
  }));
  submit(event.currentTarget, "routing-message", async (data) => {
    const routing = await api("/routing", { method: "PUT", body: { strategy: data.get("strategy"), routes } });
    state.routingVersion += 1;
    state.routing = routing;
    state.routingDirty = false;
    renderRouting();
    renderOverview();
    showMessage("routing-message", "모델 경로를 저장했습니다.");
  });
});

document.getElementById("account-form").addEventListener("submit", (event) => {
  event.preventDefault();
  submit(event.currentTarget, "account-message", async (data) => {
    state.account = await api("/account", { method: "PATCH", body: { label: data.get("label").trim() } });
    renderAccount();
    showMessage("account-message", "계정 이름을 저장했습니다.");
  });
});

document.getElementById("oauth-code-form").addEventListener("submit", (event) => {
  event.preventDefault();
  const flow = state.oauth;
  if (!flow?.login) return;
  submit(event.currentTarget, "oauth-message", async (data) => {
    try {
      const login = await api(`/providers/${encodeURIComponent(flow.provider.id)}/logins/${encodeURIComponent(flow.login.id)}/complete`, { method: "POST", body: { code: data.get("code").trim() }, signal: flow.controller.signal });
      await showOAuthStatus(flow, login);
    } catch (error) {
      if (error.name !== "AbortError" && state.oauth === flow && error.status !== 400) document.getElementById("oauth-restart").hidden = false;
      throw error;
    }
  });
});
document.getElementById("oauth-retry").addEventListener("click", () => { if (state.oauth) pollOAuth(state.oauth); });
document.getElementById("oauth-restart").addEventListener("click", () => { if (state.oauth) openOAuth(state.oauth.provider); });

document.getElementById("refresh-button").addEventListener("click", async (event) => {
  if (state.view === "routing" && state.routingDirty && !window.confirm("저장하지 않은 경로 변경 사항을 버리고 다시 불러올까요?")) return;
  const button = event.currentTarget;
  const epoch = state.epoch;
  button.disabled = true;
  showMessage("global-message");
  try {
    if (state.view === "tokens") await loadTokens();
    else if (state.view === "providers") await loadProviders();
    else if (state.view === "routing") await Promise.all([loadProviders(), loadRouting()]);
    else if (state.view === "account") { state.account = await api("/account"); renderAccount(); }
    else {
      const tasks = [loadTokens(), loadProviders()];
      if (!state.routingDirty) tasks.push(loadRouting());
      await Promise.all(tasks);
    }
  } catch (error) {
    if (error.name !== "AbortError" && epoch === state.epoch) showMessage("global-message", explain(error), true);
  } finally { if (epoch === state.epoch) button.disabled = false; }
});

document.getElementById("logout-button").addEventListener("click", async (event) => {
  const button = event.currentTarget;
  invalidateRequests();
  button.disabled = true;
  try {
    await api("/session", { method: "DELETE" });
    showAuth("로그아웃했습니다.");
    document.title = "llmproxy · 관리 콘솔";
  } catch (error) {
    if (error.name !== "AbortError") showMessage("global-message", `로그아웃하지 못했습니다. ${explain(error)}`, true);
  } finally { button.disabled = false; }
});

async function start() {
  try {
    const response = await fetch("/config.json", { cache: "no-store", credentials: "omit" });
    if (!response.ok) throw new Error("설정 파일을 불러오지 못했습니다.");
    const config = await response.json();
    const url = new URL(config.apiUrl);
    if (!["https:", "http:"].includes(url.protocol) || url.username || url.password || url.search || url.hash || url.pathname !== "/") throw new Error("API 주소 설정이 올바르지 않습니다.");
    state.apiUrl = url.origin;
    try {
      const account = await api("/account", { authenticated: false });
      await enterAccount(account);
    } catch (error) {
      if (error.name === "AbortError") return;
      showAuth(error.status === 401 ? "" : explain(error));
    }
  } catch (error) {
    document.getElementById("boot").textContent = `관리 콘솔을 시작하지 못했습니다. ${explain(error)}`;
  }
}

start();
