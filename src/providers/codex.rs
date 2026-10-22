use std::{collections::HashMap, time::Duration};

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use reqwest::{Client, StatusCode, header::HeaderMap};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use tokio::time::Instant;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use super::{auth_json, credentials_need_refresh};

use crate::{
    core::{
        ErrorKind, EventStream, FinishReason, InferenceRequest, Provider, ProviderEvent,
        ProxyError, Role, Tool, ToolChoice, Usage, WebSearchStatus,
    },
    store::{CodexCredentials, PgStore, QuotaWindow},
};

// Protocol follows openai/codex 091008729d5572712ac08964a9602eb71cf31428, codex-rs/login/src.
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub struct CodexAuth {
    client: Client,
    issuer: String,
}

pub struct DeviceLogin {
    pub verification_url: String,
    pub user_code: String,
    device_auth_id: String,
    interval: Duration,
    next_poll: Instant,
    expires_at: Instant,
}

pub enum LoginPoll {
    Pending,
    Complete(CodexCredentials),
    Expired,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: Option<u64>,
}

impl CodexAuth {
    pub fn new(client: Client, issuer: String) -> Self {
        Self {
            client,
            issuer: issuer.trim_end_matches('/').to_owned(),
        }
    }

    pub async fn start_device_login(&self) -> Result<DeviceLogin, ProxyError> {
        let response = self
            .client
            .post(format!("{}/api/accounts/deviceauth/usercode", self.issuer))
            .timeout(Duration::from_secs(30))
            .json(&json!({"client_id": CLIENT_ID}))
            .send()
            .await
            .map_err(|_| {
                ProxyError::new(ErrorKind::UpstreamAuth, "Codex login service unavailable")
            })?;
        let body = auth_json(response).await?;
        let device_auth_id = body["device_auth_id"]
            .as_str()
            .ok_or_else(|| {
                ProxyError::new(ErrorKind::UpstreamAuth, "Invalid Codex login response")
            })?
            .to_owned();
        let user_code = body
            .get("user_code")
            .or_else(|| body.get("usercode"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ProxyError::new(ErrorKind::UpstreamAuth, "Invalid Codex login response")
            })?
            .to_owned();
        let interval = body["interval"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| {
                ProxyError::new(ErrorKind::UpstreamAuth, "Invalid Codex polling interval")
            })?;
        let now = Instant::now();
        Ok(DeviceLogin {
            verification_url: format!("{}/codex/device", self.issuer),
            user_code,
            device_auth_id,
            interval: Duration::from_secs(interval),
            next_poll: now,
            expires_at: now + Duration::from_secs(15 * 60),
        })
    }

    pub async fn poll_device_login(
        &self,
        login: &mut DeviceLogin,
    ) -> Result<LoginPoll, ProxyError> {
        if Instant::now() >= login.expires_at {
            return Ok(LoginPoll::Expired);
        }
        tokio::time::sleep_until(login.next_poll.min(login.expires_at)).await;
        if Instant::now() >= login.expires_at {
            return Ok(LoginPoll::Expired);
        }
        let response = self
            .client
            .post(format!("{}/api/accounts/deviceauth/token", self.issuer))
            .timeout(
                login
                    .expires_at
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(30)),
            )
            .json(&json!({"device_auth_id": login.device_auth_id, "user_code": login.user_code}))
            .send()
            .await
            .map_err(|_| ProxyError::new(ErrorKind::UpstreamAuth, "Codex login polling failed"))?;
        login.next_poll = Instant::now() + login.interval.min(Duration::from_secs(15 * 60));
        if matches!(
            response.status(),
            StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
        ) {
            return Ok(LoginPoll::Pending);
        }
        let body = auth_json(response).await?;
        let code = body["authorization_code"].as_str().ok_or_else(|| {
            ProxyError::new(
                ErrorKind::UpstreamAuth,
                "Invalid Codex authorization response",
            )
        })?;
        let verifier = body["code_verifier"].as_str().ok_or_else(|| {
            ProxyError::new(
                ErrorKind::UpstreamAuth,
                "Invalid Codex authorization response",
            )
        })?;
        let redirect = format!("{}/deviceauth/callback", self.issuer);
        // A token exchange can consume the code even when its response is lost.
        login.expires_at = Instant::now();
        let response = self
            .client
            .post(format!("{}/oauth/token", self.issuer))
            .timeout(Duration::from_secs(30))
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", CLIENT_ID),
                ("code", code),
                ("code_verifier", verifier),
                ("redirect_uri", redirect.as_str()),
            ])
            .send()
            .await
            .map_err(|_| ProxyError::new(ErrorKind::UpstreamAuth, "Codex token exchange failed"))?;
        let tokens: TokenResponse =
            serde_json::from_value(auth_json(response).await?).map_err(|_| {
                ProxyError::new(ErrorKind::UpstreamAuth, "Invalid Codex token response")
            })?;
        Ok(LoginPoll::Complete(tokens.credentials(None)?))
    }

    pub async fn refresh(
        &self,
        credentials: &CodexCredentials,
    ) -> Result<CodexCredentials, ProxyError> {
        let response = self
            .client
            .post(format!("{}/oauth/token", self.issuer))
            .timeout(Duration::from_secs(30))
            .json(&json!({"grant_type":"refresh_token", "client_id":CLIENT_ID,
                "refresh_token":credentials.refresh_token, "refresh_reason":"proactive_expiry"}))
            .send()
            .await
            .map_err(|_| {
                ProxyError::new(ErrorKind::UpstreamAuth, "Codex credential refresh failed")
            })?;
        let tokens: TokenResponse =
            serde_json::from_value(auth_json(response).await?).map_err(|_| {
                ProxyError::new(ErrorKind::UpstreamAuth, "Invalid Codex token response")
            })?;
        tokens.credentials(Some(credentials))
    }
}

impl TokenResponse {
    fn credentials(
        self,
        previous: Option<&CodexCredentials>,
    ) -> Result<CodexCredentials, ProxyError> {
        let id_claims = self.id_token.as_deref().map(jwt_claims).transpose()?;
        let account_id = id_claims
            .as_ref()
            .and_then(|claims| claims["https://api.openai.com/auth"]["chatgpt_account_id"].as_str())
            .or_else(|| previous.map(|p| p.account_id.as_str()))
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ProxyError::new(ErrorKind::UpstreamAuth, "Codex account identifier missing")
            })?
            .to_owned();
        if previous.is_some_and(|p| p.account_id != account_id) {
            return Err(ProxyError::new(
                ErrorKind::UpstreamAuth,
                "Codex account changed during refresh",
            ));
        }
        if self.access_token.is_empty() {
            return Err(ProxyError::new(
                ErrorKind::UpstreamAuth,
                "Codex access token missing",
            ));
        }
        let expires_at = self
            .expires_in
            .and_then(|seconds| i64::try_from(seconds).ok())
            .and_then(chrono::Duration::try_seconds)
            .and_then(|duration| Utc::now().checked_add_signed(duration))
            .or_else(|| {
                jwt_claims(&self.access_token)
                    .ok()
                    .and_then(|claims| claims["exp"].as_i64())
                    .and_then(|exp| DateTime::from_timestamp(exp, 0))
            });
        let refresh_token = self
            .refresh_token
            .or_else(|| previous.map(|p| p.refresh_token.clone()))
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ProxyError::new(ErrorKind::UpstreamAuth, "Codex refresh token missing")
            })?;
        Ok(CodexCredentials {
            access_token: self.access_token,
            refresh_token,
            account_id,
            expires_at,
            updated_at: Utc::now(),
        })
    }
}

fn jwt_claims(token: &str) -> Result<Value, ProxyError> {
    let payload = token
        .split('.')
        .nth(1)
        .and_then(|s| URL_SAFE_NO_PAD.decode(s).ok())
        .ok_or_else(|| ProxyError::new(ErrorKind::UpstreamAuth, "Invalid Codex token claims"))?;
    serde_json::from_slice(&payload)
        .map_err(|_| ProxyError::new(ErrorKind::UpstreamAuth, "Invalid Codex token claims"))
}

pub struct CodexProvider {
    provider_id: String,
    store: PgStore,
    client: Client,
    auth: CodexAuth,
    responses_url: String,
    workers: TaskTracker,
}

impl CodexProvider {
    pub fn new(
        provider_id: String,
        store: PgStore,
        client: Client,
        auth: CodexAuth,
        responses_url: String,
    ) -> Self {
        Self {
            provider_id,
            store,
            client,
            auth,
            responses_url,
            workers: TaskTracker::new(),
        }
    }

    async fn refresh_locked_credentials(
        &self,
        transaction: Transaction<'static, Postgres>,
        credentials: CodexCredentials,
    ) -> Result<CodexCredentials, ProxyError> {
        let auth = self.auth.clone();
        let provider_id = self.provider_id.clone();
        // Request cancellation must not discard a rotated token before persistence.
        self.workers
            .spawn(async move {
                if credentials_need_refresh(credentials.expires_at, credentials.updated_at) {
                    let updated = auth.refresh(&credentials).await?;
                    PgStore::save_codex_credentials(transaction, &provider_id, &updated).await?;
                    tracing::info!(provider_id, "OAuth credentials refreshed");
                    Ok(updated)
                } else {
                    transaction.commit().await.map_err(ProxyError::from)?;
                    Ok(credentials)
                }
            })
            .await
            .map_err(|_| ProxyError::new(ErrorKind::UpstreamAuth, "Codex credential task failed"))?
    }
}

#[async_trait]
impl Provider for CodexProvider {
    async fn refresh_credentials(&self) -> Result<(), ProxyError> {
        let Some((transaction, credentials)) = self
            .store
            .try_lock_codex_credentials(&self.provider_id)
            .await?
        else {
            return Ok(());
        };
        self.refresh_locked_credentials(transaction, credentials)
            .await?;
        Ok(())
    }

    async fn shutdown(&self) {
        self.workers.close();
        self.workers.wait().await;
    }

    async fn infer(
        &self,
        request: InferenceRequest,
        cancellation: CancellationToken,
    ) -> Result<EventStream, ProxyError> {
        if request.max_output_tokens.is_some()
            || request.temperature.is_some()
            || request.top_p.is_some()
            || !request.stop.is_empty()
        {
            return Err(ProxyError::new(
                ErrorKind::InvalidRequest,
                "Codex does not support sampling, stop, or output limit options",
            ));
        }
        let mut input = Vec::with_capacity(request.messages.len());
        for message in request.messages {
            if matches!(message.role, Role::Tool) {
                input.push(json!({"type":"function_call_output", "call_id":message.tool_call_id, "output":message.content}));
                continue;
            }
            let (role, content_type) = match message.role {
                Role::System | Role::Developer => ("developer", "input_text"),
                Role::User => ("user", "input_text"),
                Role::Assistant => ("assistant", "output_text"),
                Role::Tool => unreachable!(),
            };
            if !message.content.is_empty() || message.tool_calls.is_empty() {
                input.push(json!({"type":"message", "role":role, "content":[{"type":content_type, "text":message.content}]}));
            }
            for call in message.tool_calls {
                input.push(json!({"type":"function_call", "call_id":call.id, "name":call.name, "arguments":call.arguments}));
            }
        }
        let tools: Vec<_> = request
            .tools
            .into_iter()
            .map(|tool| match tool {
                Tool::WebSearch => json!({"type":"web_search"}),
                Tool::Function(function) => {
                    let mut tool = json!({"type":"function", "name":function.name});
                    tool["parameters"] = function.parameters;
                    if let Some(description) = function.description {
                        tool["description"] = json!(description);
                    }
                    if let Some(strict) = function.strict {
                        tool["strict"] = json!(strict);
                    }
                    tool
                }
            })
            .collect();
        let tool_choice = match request.tool_choice {
            ToolChoice::Auto => json!("auto"),
            ToolChoice::None => json!("none"),
            ToolChoice::Required => json!("required"),
            ToolChoice::Function(name) => json!({"type":"function", "name":name}),
        };
        if cancellation.is_cancelled() {
            return Err(ProxyError::new(ErrorKind::Cancelled, "Request cancelled"));
        }
        let (transaction, credentials) = tokio::select! {
            _ = cancellation.cancelled() => return Err(ProxyError::new(ErrorKind::Cancelled, "Request cancelled")),
            locked = self.store.lock_codex_credentials(&self.provider_id) => locked?,
        };
        let credentials = tokio::select! {
            _ = cancellation.cancelled() => return Err(ProxyError::new(ErrorKind::Cancelled, "Request cancelled")),
            result = self.refresh_locked_credentials(transaction, credentials) => result?,
        };
        let mut body = json!({"model":request.model, "stream":true, "store":false,
            "parallel_tool_calls":false, "reasoning":null, "include":[]});
        body["input"] = Value::Array(input);
        body["tools"] = Value::Array(tools);
        body["tool_choice"] = tool_choice;
        let response = tokio::select! {
            _ = cancellation.cancelled() => return Err(ProxyError::new(ErrorKind::Cancelled, "Request cancelled")),
            response = self.client.post(&self.responses_url)
                .bearer_auth(&credentials.access_token).header("ChatGPT-Account-ID", &credentials.account_id)
                .header("Accept", "text/event-stream").header("originator", "codex_cli_rs")
                .json(&body).send() => response.map_err(|_| ProxyError::new(ErrorKind::Upstream, "Codex request failed"))?,
        };
        let quotas = quota_headers(response.headers());
        if !quotas.is_empty() {
            self.store.save_quota(&self.provider_id, &quotas).await?;
        }
        if !response.status().is_success() {
            let kind = match response.status() {
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ErrorKind::UpstreamAuth,
                StatusCode::TOO_MANY_REQUESTS => ErrorKind::QuotaExceeded,
                _ => ErrorKind::Upstream,
            };
            return Err(ProxyError::new(kind, "Codex request rejected"));
        }
        let store = self.store.clone();
        let provider_id = self.provider_id.clone();
        Ok(Box::pin(async_stream::try_stream! {
            let mut chunks = response.bytes_stream();
            let mut line = Vec::new();
            let mut data = Vec::new();
            let mut terminated = false;
            let mut calls = HashMap::<String, FunctionState>::new();
            let mut text_parts = HashMap::<(u64, u64), (usize, usize)>::new();
            let mut text_characters = 0usize;
            'stream: loop {
                let next = tokio::select! {
                    _ = cancellation.cancelled() => Err(ProxyError::new(ErrorKind::Cancelled, "Request cancelled")),
                    next = chunks.next() => Ok(next),
                }?;
                let Some(chunk) = next else { break; };
                let chunk = chunk.map_err(|_| ProxyError::new(ErrorKind::Upstream, "Codex stream failed"))?;
                for byte in chunk {
                    if byte != b'\n' {
                        if line.len() + data.len() >= MAX_FRAME_BYTES {
                            Err(ProxyError::new(ErrorKind::Upstream, "Codex stream event too large"))?;
                        }
                        line.push(byte);
                        continue;
                    }
                    if line.last() == Some(&b'\r') { line.pop(); }
                    if line.is_empty() {
                        if data.is_empty() { continue; }
                        let mut event: Value = serde_json::from_slice(&data)
                            .map_err(|_| ProxyError::new(ErrorKind::Upstream, "Malformed Codex stream event"))?;
                        data.clear();
                        match event["type"].as_str() {
                            Some("response.output_text.delta") => {
                                let delta = match event["delta"].take() {
                                    Value::String(delta) => delta,
                                    _ => Err(ProxyError::new(ErrorKind::Upstream, "Malformed Codex text event"))?,
                                };
                                let characters = delta.chars().count();
                                if let (Some(output), Some(content)) = (event["output_index"].as_u64(), event["content_index"].as_u64()) {
                                    let (start, length) = text_parts.entry((output, content)).or_insert((text_characters, 0));
                                    if *start + *length != text_characters {
                                        Err(ProxyError::new(ErrorKind::Upstream, "Interleaved Codex text parts are unsupported"))?;
                                    }
                                    *length += characters;
                                }
                                text_characters += characters;
                                yield ProviderEvent::TextDelta(delta);
                            }
                            Some("response.output_item.added" | "response.output_item.done") => {
                                let item = &event["item"];
                                if item["type"] == "function_call" {
                                    if let Some(delta) = function_item(item, &mut calls, event["type"] == "response.output_item.done")? {
                                        yield delta;
                                    }
                                } else if item["type"] == "web_search_call" {
                                    yield search_item(item)?;
                                }
                            }
                            Some("response.function_call_arguments.delta" | "response.function_call_arguments.done") => {
                                let key = event["item_id"].as_str().ok_or_else(|| ProxyError::new(ErrorKind::Upstream, "Invalid Codex tool event"))?;
                                let call = calls.get_mut(key).ok_or_else(|| ProxyError::new(ErrorKind::Upstream, "Codex tool arguments without call"))?;
                                let done = event["type"] == "response.function_call_arguments.done";
                                let arguments = event[if done {"arguments"} else {"delta"}].as_str()
                                    .ok_or_else(|| ProxyError::new(ErrorKind::Upstream, "Invalid Codex tool arguments"))?;
                                if let Some(delta) = call.append(arguments, done)? { yield delta; }
                            }
                            Some("response.web_search_call.in_progress" | "response.web_search_call.searching" | "response.web_search_call.completed") => {
                                let id = event["item_id"].as_str().ok_or_else(|| ProxyError::new(ErrorKind::Upstream, "Invalid Codex web search event"))?.to_owned();
                                let status = match event["type"].as_str() {
                                    Some("response.web_search_call.in_progress") => WebSearchStatus::InProgress,
                                    Some("response.web_search_call.searching") => WebSearchStatus::Searching,
                                    _ => WebSearchStatus::Completed,
                                };
                                yield ProviderEvent::WebSearch { id, query: None, status };
                            }
                            Some("response.output_text.annotation.added") => {
                                let annotation = &event["annotation"];
                                if annotation["type"] == "url_citation" {
                                    let part = event["output_index"].as_u64().zip(event["content_index"].as_u64())
                                        .and_then(|key| text_parts.get(&key))
                                        .ok_or_else(|| ProxyError::new(ErrorKind::Upstream, "Codex citation has no text part"))?;
                                    yield citation(annotation, part.0, part.1)?;
                                }
                            }
                            Some("codex.rate_limits") => {
                                let windows = quota_event(&event);
                                if !windows.is_empty() { store.save_quota(&provider_id, &windows).await?; }
                            }
                            Some("response.completed" | "response.incomplete" | "response.failed") => {
                                if let Some(output) = event["response"]["output"].as_array() {
                                    for item in output.iter().filter(|item| item["type"] == "function_call") {
                                        if let Some(delta) = function_item(item, &mut calls, true)? {
                                            yield delta;
                                        }
                                    }
                                }
                                if let Some(usage) = event["response"].get("usage").filter(|usage| !usage.is_null()) {
                                    let input_tokens = usage["input_tokens"].as_u64();
                                    let output_tokens = usage["output_tokens"].as_u64();
                                    if let (Some(input_tokens), Some(output_tokens)) = (input_tokens, output_tokens) {
                                        yield ProviderEvent::Usage(Usage { input_tokens, output_tokens });
                                    } else {
                                        Err(ProxyError::new(ErrorKind::Upstream, "Invalid Codex usage"))?;
                                    }
                                }
                                let finish = match event["type"].as_str() {
                                    Some("response.completed") if event["response"].is_object() => {
                                        if calls.values().any(|call| !call.completed) {
                                            Err(ProxyError::new(ErrorKind::Upstream, "Codex response contains unfinished tool calls"))?;
                                        }
                                        if calls.is_empty() { FinishReason::Stop } else { FinishReason::ToolCalls }
                                    },
                                    Some("response.incomplete") if event["response"]["incomplete_details"]["reason"] == "max_output_tokens" => FinishReason::Length,
                                    _ => Err(ProxyError::new(ErrorKind::Upstream, "Codex generation failed or incomplete"))?,
                                };
                                yield ProviderEvent::Finished(finish);
                                terminated = true;
                                break 'stream;
                            }
                            Some("error") => Err(ProxyError::new(ErrorKind::Upstream, "Codex generation failed"))?,
                            _ => {},
                        }
                    } else if let Some(value) = line.strip_prefix(b"data:") {
                        let value = value.strip_prefix(b" ").unwrap_or(value);
                        if !data.is_empty() { data.push(b'\n'); }
                        data.extend_from_slice(value);
                    }
                    line.clear();
                }
            }
            if !terminated { Err(ProxyError::new(ErrorKind::Upstream, "Codex stream ended without a terminal event"))?; }
        }))
    }
}

fn quota_headers(headers: &HeaderMap) -> Vec<QuotaWindow> {
    let mut windows = Vec::new();
    for (name, value) in headers {
        let Some(prefix) = name
            .as_str()
            .strip_suffix("-used-percent")
            .filter(|prefix| {
                prefix.starts_with("x-codex-")
                    && (prefix.ends_with("-primary") || prefix.ends_with("-secondary"))
            })
        else {
            continue;
        };
        let Some(used) = value
            .to_str()
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|used| used.is_finite() && *used >= 0.0)
        else {
            continue;
        };
        let resets_at = headers
            .get(format!("{prefix}-reset-at"))
            .and_then(|value| value.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok())
            .and_then(|seconds| DateTime::from_timestamp(seconds, 0));
        windows.push(QuotaWindow {
            key: prefix.trim_start_matches("x-").replace('-', "_"),
            observed_at: Utc::now(),
            remaining_fraction: (1.0 - used / 100.0).max(0.0),
            resets_at,
        });
    }
    windows
}

fn quota_event(event: &Value) -> Vec<QuotaWindow> {
    let meter = event["metered_limit_name"]
        .as_str()
        .or_else(|| event["limit_name"].as_str())
        .unwrap_or("codex")
        .replace('-', "_");
    let mut windows = Vec::new();
    for name in ["primary", "secondary"] {
        let window = &event["rate_limits"][name];
        let Some(used) = window["used_percent"]
            .as_f64()
            .filter(|used| used.is_finite() && *used >= 0.0)
        else {
            continue;
        };
        windows.push(QuotaWindow {
            key: format!("{meter}_{name}"),
            observed_at: Utc::now(),
            remaining_fraction: (1.0 - used / 100.0).max(0.0),
            resets_at: window["reset_at"]
                .as_i64()
                .and_then(|seconds| DateTime::from_timestamp(seconds, 0)),
        });
    }
    windows
}

struct FunctionState {
    index: usize,
    id: String,
    name: String,
    arguments: String,
    completed: bool,
}

impl FunctionState {
    fn append(&mut self, arguments: &str, done: bool) -> Result<Option<ProviderEvent>, ProxyError> {
        let delta = if done {
            arguments.strip_prefix(&self.arguments).ok_or_else(|| {
                ProxyError::new(ErrorKind::Upstream, "Inconsistent Codex tool arguments")
            })?
        } else {
            if self.completed {
                return Err(ProxyError::new(
                    ErrorKind::Upstream,
                    "Codex tool arguments after completion",
                ));
            }
            arguments
        };
        if self.arguments.len() + delta.len() > MAX_FRAME_BYTES {
            return Err(ProxyError::new(
                ErrorKind::Upstream,
                "Codex tool arguments too large",
            ));
        }
        self.completed |= done;
        if delta.is_empty() {
            return Ok(None);
        }
        let delta = delta.to_owned();
        self.arguments.push_str(&delta);
        Ok(Some(ProviderEvent::ToolCallDelta {
            index: self.index,
            id: None,
            name: None,
            arguments: delta,
        }))
    }
}

fn function_item(
    item: &Value,
    calls: &mut HashMap<String, FunctionState>,
    done: bool,
) -> Result<Option<ProviderEvent>, ProxyError> {
    let (Some(key), Some(id), Some(name), Some(arguments)) = (
        item["id"].as_str(),
        item["call_id"].as_str(),
        item["name"].as_str(),
        item["arguments"].as_str(),
    ) else {
        return Err(ProxyError::new(
            ErrorKind::Upstream,
            "Invalid Codex function call",
        ));
    };
    if let Some(call) = calls.get_mut(key) {
        if call.id != id || call.name != name {
            return Err(ProxyError::new(
                ErrorKind::Upstream,
                "Inconsistent Codex function call",
            ));
        }
        return call.append(arguments, done);
    }
    if arguments.len() > MAX_FRAME_BYTES {
        return Err(ProxyError::new(
            ErrorKind::Upstream,
            "Codex tool arguments too large",
        ));
    }
    let index = calls.len();
    calls.insert(
        key.to_owned(),
        FunctionState {
            index,
            id: id.to_owned(),
            name: name.to_owned(),
            arguments: arguments.to_owned(),
            completed: done,
        },
    );
    Ok(Some(ProviderEvent::ToolCallDelta {
        index,
        id: Some(id.to_owned()),
        name: Some(name.to_owned()),
        arguments: arguments.to_owned(),
    }))
}

fn search_item(item: &Value) -> Result<ProviderEvent, ProxyError> {
    let id = item["id"]
        .as_str()
        .ok_or_else(|| ProxyError::new(ErrorKind::Upstream, "Invalid Codex web search"))?
        .to_owned();
    let status = match item["status"].as_str() {
        Some("in_progress") => WebSearchStatus::InProgress,
        Some("searching") => WebSearchStatus::Searching,
        Some("completed") => WebSearchStatus::Completed,
        _ => {
            return Err(ProxyError::new(
                ErrorKind::Upstream,
                "Invalid Codex web search status",
            ));
        }
    };
    let query = item["action"]["query"].as_str().map(str::to_owned);
    Ok(ProviderEvent::WebSearch { id, query, status })
}

fn citation(
    annotation: &Value,
    offset: usize,
    text_length: usize,
) -> Result<ProviderEvent, ProxyError> {
    let (Some(url), Some(title), Some(start), Some(end)) = (
        annotation["url"].as_str(),
        annotation["title"].as_str(),
        annotation["start_index"].as_u64(),
        annotation["end_index"].as_u64(),
    ) else {
        return Err(ProxyError::new(
            ErrorKind::Upstream,
            "Invalid Codex citation",
        ));
    };
    let start_index = usize::try_from(start)
        .map_err(|_| ProxyError::new(ErrorKind::Upstream, "Invalid Codex citation"))?;
    let end_index = usize::try_from(end)
        .map_err(|_| ProxyError::new(ErrorKind::Upstream, "Invalid Codex citation"))?;
    if start_index > end_index || end_index > text_length {
        return Err(ProxyError::new(
            ErrorKind::Upstream,
            "Invalid Codex citation",
        ));
    }
    Ok(ProviderEvent::Citation {
        url: url.to_owned(),
        title: title.to_owned(),
        start_index: start_index + offset,
        end_index: end_index + offset,
    })
}
