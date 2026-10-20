use std::{
    collections::{HashMap, HashSet},
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    io::AsyncWriteExt,
    process::{ChildStdin, Command},
    sync::mpsc,
};
use tokio_util::{
    codec::{FramedRead, LinesCodec},
    sync::CancellationToken,
    task::TaskTracker,
};
use uuid::Uuid;

use crate::core::{
    ErrorKind, EventStream, FinishReason, InferenceRequest, Provider, ProviderEvent, ProxyError,
    Role, Tool, ToolChoice, Usage, WebSearchStatus,
};

use super::{auth_json, credentials_need_refresh};
use crate::store::{PgStore, QuotaWindow};

const DEFAULT_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const DEFAULT_SCOPES: &str = "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload user:plugins";

pub struct ClaudeAuth {
    client: Client,
    token_url: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: i64,
    refresh_token: Option<String>,
    scope: Option<String>,
    refresh_token_expires_in: Option<i64>,
}

impl ClaudeAuth {
    pub fn new(client: Client, token_url: String) -> Self {
        Self { client, token_url }
    }

    async fn refresh(
        &self,
        oauth: &mut serde_json::Map<String, Value>,
        refresh_token: &str,
    ) -> Result<(), ProxyError> {
        let invalid =
            || ProxyError::new(ErrorKind::UpstreamAuth, "Invalid Claude OAuth credentials");
        let client_id = match oauth.get("clientId") {
            None | Some(Value::Null) => DEFAULT_CLIENT_ID,
            Some(value) => value
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or_else(invalid)?,
        };
        let scopes = match oauth.get("scopes") {
            None | Some(Value::Null) => DEFAULT_SCOPES.to_owned(),
            Some(Value::Array(values)) if values.is_empty() => DEFAULT_SCOPES.to_owned(),
            Some(Value::Array(values)) => values
                .iter()
                .map(|scope| scope.as_str().ok_or_else(invalid))
                .collect::<Result<Vec<_>, _>>()?
                .join(" "),
            Some(_) => return Err(invalid()),
        };
        // Matches the refresh-token exchange in the official Claude Code 2.1.304 CLI.
        let response = self.client.post(&self.token_url)
            .timeout(Duration::from_secs(30))
            .json(&json!({"grant_type":"refresh_token","refresh_token":refresh_token,"client_id":client_id,"scope":scopes}))
            .send().await.map_err(|_| ProxyError::new(ErrorKind::UpstreamAuth, "Claude credential refresh failed"))?;
        let invalid_response =
            || ProxyError::new(ErrorKind::UpstreamAuth, "Invalid Claude token response");
        let tokens: TokenResponse =
            serde_json::from_value(auth_json(response).await?).map_err(|_| invalid_response())?;
        if tokens.access_token.is_empty()
            || tokens.expires_in <= 0
            || tokens.refresh_token.as_ref().is_some_and(String::is_empty)
        {
            return Err(invalid_response());
        }
        let now = Utc::now();
        let expires = chrono::Duration::try_seconds(tokens.expires_in)
            .and_then(|duration| now.checked_add_signed(duration))
            .ok_or_else(invalid_response)?;
        let refresh_expires = tokens
            .refresh_token_expires_in
            .map(|seconds| {
                chrono::Duration::try_seconds(seconds)
                    .and_then(|duration| now.checked_add_signed(duration))
                    .ok_or_else(invalid_response)
            })
            .transpose()?;
        oauth.insert("accessToken".into(), json!(tokens.access_token));
        oauth.insert("expiresAt".into(), json!(expires.timestamp_millis()));
        if let Some(refresh_token) = tokens.refresh_token {
            oauth.insert("refreshToken".into(), json!(refresh_token));
        }
        if let Some(scope) = tokens.scope {
            oauth.insert(
                "scopes".into(),
                json!(scope.split_whitespace().collect::<Vec<_>>()),
            );
        }
        if let Some(expires) = refresh_expires {
            oauth.insert(
                "refreshTokenExpiresAt".into(),
                json!(expires.timestamp_millis()),
            );
        }
        Ok(())
    }
}

pub struct ClaudeProvider {
    id: String,
    store: PgStore,
    workers: TaskTracker,
    executable: PathBuf,
    auth: ClaudeAuth,
}

impl ClaudeProvider {
    pub fn new(id: String, executable: PathBuf, store: PgStore, auth: ClaudeAuth) -> Self {
        Self {
            executable,
            auth,
            id,
            store,
            workers: TaskTracker::new(),
        }
    }
}

#[async_trait]
impl Provider for ClaudeProvider {
    async fn refresh_credentials(&self) -> Result<(), ProxyError> {
        let Some((transaction, mut credentials)) =
            self.store.try_lock_claude_credentials(&self.id).await?
        else {
            return Ok(());
        };
        let oauth = credentials
            .credentials
            .get_mut("claudeAiOauth")
            .and_then(Value::as_object_mut);
        let eligible = oauth.and_then(|oauth| {
            let token = oauth
                .get("refreshToken")
                .and_then(Value::as_str)
                .filter(|token| !token.is_empty())?
                .to_owned();
            Some((oauth, token))
        });
        let Some((oauth, refresh_token)) = eligible else {
            transaction.commit().await?;
            return Ok(());
        };
        let expires_at = oauth
            .get("expiresAt")
            .and_then(Value::as_i64)
            .and_then(DateTime::from_timestamp_millis);
        if !credentials_need_refresh(expires_at, credentials.updated_at) {
            transaction.commit().await?;
            return Ok(());
        }
        self.auth.refresh(oauth, &refresh_token).await?;
        PgStore::save_claude_credentials(transaction, &self.id, &credentials).await
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
                "Claude CLI does not support these sampling or output-limit options",
            ));
        }
        if matches!(
            request.tool_choice,
            ToolChoice::Required | ToolChoice::Function(_)
        ) {
            return Err(ProxyError::new(
                ErrorKind::InvalidRequest,
                "Claude CLI supports tool_choice auto or none",
            ));
        }
        let tools_enabled = !matches!(request.tool_choice, ToolChoice::None);
        let search_enabled = tools_enabled
            && request
                .tools
                .iter()
                .any(|tool| matches!(tool, Tool::WebSearch));
        let mut functions = Vec::new();
        let mut function_names = HashSet::new();
        for tool in &request.tools {
            if let Tool::Function(function) = tool {
                if function.strict == Some(true) {
                    return Err(ProxyError::new(
                        ErrorKind::InvalidRequest,
                        "Claude CLI cannot guarantee strict function schemas",
                    ));
                }
                if tools_enabled {
                    function_names.insert(function.name.clone());
                    functions.push(json!({"name":function.name,"description":function.description,"inputSchema":function.parameters}));
                }
            }
        }
        let (transaction, mut credentials) = tokio::select! {
            _ = cancellation.cancelled() => return Err(ProxyError::new(ErrorKind::Cancelled, "Request cancelled")),
            locked = self.store.lock_claude_credentials(&self.id) => locked?,
        };
        let work = tempfile::tempdir().map_err(|_| {
            ProxyError::new(
                ErrorKind::Unavailable,
                "Cannot create Claude working directory",
            )
        })?;
        let config_dir = work.path().join("config");
        std::fs::create_dir(&config_dir).map_err(|_| {
            ProxyError::new(
                ErrorKind::Unavailable,
                "Cannot prepare Claude authentication",
            )
        })?;
        let mut auth_config =
            json!({"hasCompletedOnboarding":credentials.has_completed_onboarding});
        if let Some(account) = &credentials.oauth_account {
            auth_config["oauthAccount"] = account.clone();
        }
        if let Some(user_id) = &credentials.user_id {
            auth_config["userID"] = json!(user_id);
        }
        for (name, value) in [
            (".credentials.json", &credentials.credentials),
            (".claude.json", &auth_config),
        ] {
            std::fs::write(config_dir.join(name), value.to_string()).map_err(|_| {
                ProxyError::new(
                    ErrorKind::Unavailable,
                    "Cannot prepare Claude authentication",
                )
            })?;
        }
        let mut system = Vec::new();
        let mut conversation = Vec::<(String, Vec<Value>)>::new();
        for message in request.messages {
            match message.role {
                Role::System | Role::Developer => {
                    if !conversation.is_empty() {
                        return Err(ProxyError::new(
                            ErrorKind::InvalidRequest,
                            "Claude requires system and developer messages before the conversation",
                        ));
                    }
                    system.push(message.content);
                }
                Role::User | Role::Assistant | Role::Tool => {
                    let role = if matches!(message.role, Role::Assistant) {
                        "assistant"
                    } else {
                        "user"
                    };
                    let mut blocks = Vec::new();
                    if matches!(message.role, Role::Tool) {
                        blocks.push(json!({"type":"tool_result","tool_use_id":message.tool_call_id,"content":[{"type":"text","text":message.content}]}));
                    } else {
                        if !message.content.is_empty() {
                            blocks.push(json!({"type":"text","text":message.content}));
                        }
                        for call in message.tool_calls {
                            let input: Value =
                                serde_json::from_str(&call.arguments).map_err(|_| {
                                    ProxyError::new(
                                        ErrorKind::InvalidRequest,
                                        "Function call arguments must be JSON",
                                    )
                                })?;
                            blocks.push(json!({"type":"tool_use","id":call.id,"name":format!("mcp__client__{}",call.name),"input":input}));
                        }
                    }
                    if let Some((previous_role, previous_blocks)) = conversation.last_mut()
                        && previous_role == role
                    {
                        previous_blocks.extend(blocks);
                        continue;
                    }
                    conversation.push((role.into(), blocks));
                }
            }
        }
        let (last_role, last_content) = conversation.pop().ok_or_else(|| {
            ProxyError::new(ErrorKind::InvalidRequest, "A user message is required")
        })?;
        if last_role != "user" {
            return Err(ProxyError::new(
                ErrorKind::InvalidRequest,
                "Claude requires a final user message or function result",
            ));
        }
        let resume_with_results = last_content
            .iter()
            .any(|block| block["type"] == "tool_result");
        // Resume repair must see the matching results before Claude loads the transcript.
        let last_content = if resume_with_results {
            conversation.push((last_role, last_content));
            None
        } else {
            Some(last_content)
        };
        let session = Uuid::new_v4().to_string();
        let mut parent = None::<String>;
        let mut history = String::new();
        for (role, blocks) in conversation {
            let id = Uuid::new_v4().to_string();
            let message = if role == "assistant" {
                json!({"role":role,"content":blocks,"id":format!("msg_{id}"),"type":"message","model":request.model,"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":0,"output_tokens":0}})
            } else {
                json!({"role":role,"content":blocks})
            };
            let record = json!({"type":role,"message":message,"uuid":id,"parentUuid":parent,"sessionId":session,"timestamp":chrono::Utc::now(),"isSidechain":false,"cwd":work.path(),"userType":"external"});
            history.push_str(&record.to_string());
            history.push('\n');
            parent = Some(id);
        }
        let system_path = work.path().join("system.txt");
        std::fs::write(
            &system_path,
            if system.is_empty() {
                "You are a helpful assistant.".into()
            } else {
                system.join("\n\n")
            },
        )
        .map_err(|_| ProxyError::new(ErrorKind::Unavailable, "Cannot prepare Claude request"))?;
        let mcp = if functions.is_empty() {
            json!({"mcpServers":{}})
        } else {
            json!({"mcpServers":{"client":{"type":"sdk","name":"client"}}})
        };
        let mut command = Command::new(&self.executable);
        command
            .args([
                "-p",
                "--safe-mode",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--include-partial-messages",
                "--verbose",
                "--no-session-persistence",
                "--setting-sources",
                "",
                "--strict-mcp-config",
                "--permission-mode",
                "dontAsk",
                "--permission-prompts",
                "none",
                "--disable-slash-commands",
                "--tools",
            ])
            .arg(if search_enabled { "WebSearch" } else { "" })
            .arg("--mcp-config")
            .arg(mcp.to_string())
            .arg("--model")
            .arg(&request.model)
            .arg("--system-prompt-file")
            .arg(system_path)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", work.path())
            .env("CLAUDE_CONFIG_DIR", &config_dir)
            .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
            .env("DISABLE_AUTOUPDATER", "1")
            .current_dir(work.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if !history.is_empty() {
            let path = work.path().join("history.jsonl");
            std::fs::write(&path, history).map_err(|_| {
                ProxyError::new(ErrorKind::Unavailable, "Cannot prepare Claude history")
            })?;
            command.arg("--resume").arg(path);
            if resume_with_results {
                command.env("CLAUDE_CODE_RESUME_INTERRUPTED_TURN", "1");
            }
        }
        let mut child = command.spawn().map_err(|_| {
            ProxyError::new(ErrorKind::Unavailable, "Cannot start Claude executable")
        })?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let (sender, receiver) = mpsc::channel(16);
        let id = self.id.clone();
        let store = self.store.clone();
        self.workers.spawn(async move {
            let _work = work;
            let process = &mut child;
            let mut quota = HashMap::new();
            let quota_updates = &mut quota;
            let stream = async_stream::try_stream! {
            // The SDK control protocol exposes client functions without executing host code.
            write_message(&mut stdin, &json!({"type":"control_request","request_id":"proxy-init","request":{"subtype":"initialize","hooks":null}})).await?;
            let mut stdin = Some(stdin);
            let mut lines = FramedRead::new(stdout, LinesCodec::new_with_max_length(8 * 1024 * 1024));
            let mut result = None;
            let mut search_ids = HashSet::new();
            let mut pending_calls = Vec::<(String, String, String)>::new();
            let mut usages = HashMap::<String, Usage>::new();
            let mut current_message = None::<String>;
            loop {
                let line = match lines.next().await {
                    Some(Ok(line)) => Some(line),
                    Some(Err(_)) => Err(ProxyError::new(ErrorKind::Upstream, "Invalid Claude output frame"))?,
                    None => None,
                };
                let Some(line) = line else { break; };
                let message: Value = serde_json::from_str(&line).map_err(|_| ProxyError::new(ErrorKind::Upstream, "Invalid Claude output JSON"))?;
                match message["type"].as_str() {
                    Some("rate_limit_event") => {
                        let observed_at = Utc::now();
                        for name in ["five_hour", "seven_day"] {
                            let window = &message["rate_limit_info"]["unifiedWindows"][name];
                            let Some(utilization) = window["utilization"].as_f64().filter(|value| value.is_finite() && *value >= 0.0) else { continue; };
                            let Some(resets_at) = window["resetsAt"].as_i64().and_then(|seconds| DateTime::from_timestamp(seconds, 0)) else { continue; };
                            quota_updates.insert(name, QuotaWindow {
                                key: format!("claude_{name}"), observed_at,
                                remaining_fraction: (1.0 - utilization).max(0.0),
                                resets_at: Some(resets_at),
                            });
                        }
                    }
                    Some("control_response") if message["response"]["request_id"] == "proxy-init" => {
                        if message["response"]["subtype"] != "success" {
                            Err(ProxyError::new(ErrorKind::Upstream, "Claude initialization failed"))?;
                        }
                        if let Some(last_content) = &last_content {
                        write_message(stdin.as_mut().ok_or_else(|| ProxyError::new(ErrorKind::Upstream, "Unexpected Claude initialization"))?, &json!({"type":"user","message":{"role":"user","content":last_content},"parent_tool_use_id":null,"session_id":""})).await?;
                        }
                    }
                    Some("control_request") => {
                        let request = &message["request"];
                        let rpc = &request["message"];
                        let rpc_result = match (request["subtype"].as_str(), request["server_name"].as_str(), rpc["method"].as_str()) {
                            (Some("mcp_message"), Some("client"), Some("initialize")) => json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"llmproxy-client","version":"1"}}),
                            (Some("mcp_message"), Some("client"), Some("notifications/initialized")) => json!({}),
                            (Some("mcp_message"), Some("client"), Some("tools/list")) => json!({"tools":functions}),
                            (Some("mcp_message"), Some("client"), Some("tools/call")) => continue,
                            _ => Err(ProxyError::new(ErrorKind::Upstream, "Unexpected Claude control request"))?,
                        };
                        write_message(stdin.as_mut().ok_or_else(|| ProxyError::new(ErrorKind::Upstream, "Unexpected Claude control request"))?, &json!({"type":"control_response","response":{"subtype":"success","request_id":message["request_id"],"response":{"mcp_response":{"jsonrpc":"2.0","id":rpc["id"],"result":rpc_result}}}})).await?;
                    }
                    Some("stream_event") => {
                        let event = &message["event"];
                        match event["type"].as_str() {
                            Some("message_start") => {
                                let id = event["message"]["id"].as_str().ok_or_else(|| ProxyError::new(ErrorKind::Upstream, "Invalid Claude message ID"))?;
                                current_message = Some(id.into());
                                if let Some(usage) = parse_usage(&event["message"]["usage"])? { usages.insert(id.into(), usage); }
                            }
                            Some("content_block_delta") if event["delta"]["type"] == "text_delta" => {
                                let text = event["delta"]["text"].as_str().ok_or_else(|| ProxyError::new(ErrorKind::Upstream, "Invalid Claude text delta"))?;
                                yield ProviderEvent::TextDelta(text.into());
                            }
                            Some("message_delta") => {
                                if let (Some(usage), Some(output)) = (current_message.as_ref().and_then(|id| usages.get_mut(id)), event["usage"].get("output_tokens")) {
                                    usage.output_tokens = output.as_u64().ok_or_else(|| ProxyError::new(ErrorKind::Upstream, "Invalid Claude usage"))?;
                                }
                            }
                            Some("message_stop") => {
                                if !usages.is_empty() {
                                    let total = usages.values().try_fold(Usage{input_tokens:0,output_tokens:0}, |total, value| {
                                        Ok::<_, ProxyError>(Usage {
                                            input_tokens:total.input_tokens.checked_add(value.input_tokens).ok_or_else(|| ProxyError::new(ErrorKind::Upstream, "Invalid Claude usage"))?,
                                            output_tokens:total.output_tokens.checked_add(value.output_tokens).ok_or_else(|| ProxyError::new(ErrorKind::Upstream, "Invalid Claude usage"))?,
                                        })
                                    })?;
                                    yield ProviderEvent::Usage(total);
                                }
                                if !pending_calls.is_empty() {
                                    // Wait for message_stop to retain parallel calls and final token counts.
                                    process.kill().await.map_err(|_| ProxyError::new(ErrorKind::Upstream, "Cannot stop Claude at function handoff"))?;
                                    for (index,(id,name,arguments)) in pending_calls.drain(..).enumerate() {
                                        yield ProviderEvent::ToolCallDelta {index,id:Some(id),name:Some(name),arguments};
                                    }
                                    yield ProviderEvent::Finished(FinishReason::ToolCalls);
                                    return;
                                }
                            }
                            _ => {}
                        }
                    }
                    Some("assistant") => {
                        let blocks = message["message"]["content"].as_array().ok_or_else(|| ProxyError::new(ErrorKind::Upstream, "Invalid Claude assistant message"))?;
                        if let (Some(id), Some(usage)) = (message["message"]["id"].as_str(), parse_usage(&message["message"]["usage"])?) {
                            usages.insert(id.into(), usage);
                        }
                        for block in blocks {
                            if block["type"] != "tool_use" { continue; }
                            let name = block["name"].as_str().ok_or_else(|| ProxyError::new(ErrorKind::Upstream, "Invalid Claude tool call"))?;
                            let id = block["id"].as_str().ok_or_else(|| ProxyError::new(ErrorKind::Upstream, "Invalid Claude tool call"))?;
                            if name == "WebSearch" && search_enabled {
                                search_ids.insert(id.to_owned());
                                yield ProviderEvent::WebSearch { id:id.into(),query:block["input"]["query"].as_str().map(str::to_owned),status:WebSearchStatus::Searching };
                            } else if let Some(name) = name.strip_prefix("mcp__client__").filter(|name| function_names.contains(*name)) {
                                if !pending_calls.iter().any(|(existing,_,_)| existing == id) {
                                    pending_calls.push((id.to_owned(), name.to_owned(), block["input"].to_string()));
                                }
                            } else {
                                Err(ProxyError::new(ErrorKind::Upstream, "Claude requested an unregistered tool"))?;
                            }
                        }
                    }
                    Some("user") => {
                        if let Some(blocks) = message["message"]["content"].as_array() {
                            for block in blocks {
                                if let Some(id) = block["tool_use_id"].as_str().filter(|id| search_ids.remove(*id)) {
                                    yield ProviderEvent::WebSearch {id:id.into(),query:None,status:WebSearchStatus::Completed};
                                }
                            }
                        }
                    }
                    Some("result") => {
                        if result.is_some() || message["is_error"] != false || message["subtype"] != "success" {
                            Err(ProxyError::new(ErrorKind::Upstream, "Claude request failed"))?;
                        }
                        if let Some(usage) = parse_usage(&message["usage"])? { yield ProviderEvent::Usage(usage); }
                        let finish = match message["stop_reason"].as_str() {
                            Some("max_tokens") => FinishReason::Length,
                            Some("end_turn" | "stop_sequence") | None => FinishReason::Stop,
                            _ => Err(ProxyError::new(ErrorKind::Upstream, "Unexpected Claude stop reason"))?,
                        };
                        result = Some(finish);
                        drop(stdin.take());
                    }
                    _ => {}
                }
            }
            let status = process.wait().await.map_err(|_| ProxyError::new(ErrorKind::Upstream, "Cannot wait for Claude process"))?;
            if !status.success() { Err(ProxyError::new(ErrorKind::Upstream, "Claude process failed"))?; }
            let finish = result.ok_or_else(|| ProxyError::new(ErrorKind::Upstream, "Claude ended before completion"))?;
            yield ProviderEvent::Finished(finish);
        };
            let mut stream = Box::pin(stream);
            let terminal = loop {
                let event = tokio::select! {
                    _ = sender.closed() => break None,
                    _ = cancellation.cancelled() => break Some(Err(ProxyError::new(ErrorKind::Cancelled, "Request cancelled"))),
                    event = stream.next() => event,
                };
                match event {
                    Some(Ok(ProviderEvent::Finished(reason))) => break Some(Ok(ProviderEvent::Finished(reason))),
                    Some(Err(error)) => break Some(Err(error)),
                    Some(Ok(event)) => {
                        tokio::select! {
                            _ = cancellation.cancelled() => break Some(Err(ProxyError::new(ErrorKind::Cancelled, "Request cancelled"))),
                            result = sender.send(Ok(event)) => if result.is_err() { break None; },
                        }
                    }
                    None => break Some(Err(ProxyError::new(ErrorKind::Upstream, "Claude ended before completion"))),
                }
            };
            drop(stream);
            // Reap before reading auth files; even dropped streams must save rotation.
            let stopped = match child.try_wait() {
                Ok(Some(_)) => Ok(()),
                Ok(None) => child.kill().await,
                Err(error) => Err(error),
            };
            let saved = async {
                stopped.map_err(|_| ProxyError::new(ErrorKind::Upstream, "Cannot stop Claude process"))?;
                credentials.credentials = read_auth_json(&config_dir.join(".credentials.json"))?;
                let updated = read_auth_json(&config_dir.join(".claude.json"))?;
                if let Some(account) = updated.get("oauthAccount") {
                    if !account.is_object() {
                        return Err(ProxyError::new(ErrorKind::Persistence, "Invalid Claude account configuration"));
                    }
                    credentials.oauth_account = Some(account.clone());
                }
                if let Some(user_id) = updated.get("userID") {
                    credentials.user_id = Some(user_id.as_str().ok_or_else(|| ProxyError::new(ErrorKind::Persistence, "Invalid Claude user configuration"))?.to_owned());
                }
                if let Some(onboarding) = updated.get("hasCompletedOnboarding") {
                    credentials.has_completed_onboarding = onboarding.as_bool().ok_or_else(|| ProxyError::new(ErrorKind::Persistence, "Invalid Claude onboarding configuration"))?;
                }
                PgStore::save_claude_credentials(transaction, &id, &credentials).await?;
                // Release the auth connection and retain rotation even if quota saving fails.
                if !quota.is_empty() {
                    store.save_quota(&id, &quota.into_values().collect::<Vec<_>>()).await?;
                }
                Ok::<_, ProxyError>(())
            }.await;
            let terminal = match saved {
                Err(error) => {
                    tracing::error!(provider_id=%id, error_kind=error.kind.as_str(), "Cannot persist Claude state");
                    Some(Err(error))
                }
                Ok(()) => terminal,
            };
            drop(_work);
            if let Some(terminal) = terminal {
                tokio::select! {
                    permit = sender.reserve() => if let Ok(permit) = permit { permit.send(terminal); },
                    _ = cancellation.cancelled() => { let _ = sender.try_send(terminal); },
                }
            }
        });
        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(
            receiver,
        )))
    }
}

fn read_auth_json(path: &Path) -> Result<Value, ProxyError> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(1024 * 1024 + 1).read_to_end(&mut bytes))
        .map_err(|_| {
            ProxyError::new(ErrorKind::Persistence, "Cannot read Claude authentication")
        })?;
    if bytes.len() > 1024 * 1024 {
        return Err(ProxyError::new(
            ErrorKind::Persistence,
            "Claude authentication file is too large",
        ));
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
        ProxyError::new(ErrorKind::Persistence, "Invalid Claude authentication JSON")
    })?;
    if !value.is_object() {
        return Err(ProxyError::new(
            ErrorKind::Persistence,
            "Invalid Claude authentication document",
        ));
    }
    Ok(value)
}

async fn write_message(stdin: &mut ChildStdin, message: &Value) -> Result<(), ProxyError> {
    let mut line = message.to_string();
    line.push('\n');
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|_| ProxyError::new(ErrorKind::Upstream, "Cannot write Claude request"))
}

fn parse_usage(value: &Value) -> Result<Option<Usage>, ProxyError> {
    if value.is_null() {
        return Ok(None);
    }
    let input = value["input_tokens"]
        .as_u64()
        .ok_or_else(|| ProxyError::new(ErrorKind::Upstream, "Invalid Claude usage"))?;
    let output = value["output_tokens"]
        .as_u64()
        .ok_or_else(|| ProxyError::new(ErrorKind::Upstream, "Invalid Claude usage"))?;
    let mut total = input;
    for key in ["cache_read_input_tokens", "cache_creation_input_tokens"] {
        if let Some(value) = value.get(key) {
            total =
                total
                    .checked_add(value.as_u64().ok_or_else(|| {
                        ProxyError::new(ErrorKind::Upstream, "Invalid Claude usage")
                    })?)
                    .ok_or_else(|| ProxyError::new(ErrorKind::Upstream, "Invalid Claude usage"))?;
        }
    }
    Ok(Some(Usage {
        input_tokens: total,
        output_tokens: output,
    }))
}
