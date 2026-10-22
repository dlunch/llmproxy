use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Instant,
};

mod output;
use output::{chat_stream, collect, responses_stream};

use axum::{
    Extension, Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    core::{
        ErrorKind, FunctionDefinition, InferenceRequest, Message, Principal, ProxyError, Role,
        Tool, ToolCall, ToolChoice,
    },
    gateway::Gateway,
    metrics::{Metrics, Stage},
    store::PgStore,
};

#[derive(Clone)]
pub struct ApiState {
    pub gateway: Arc<Gateway>,
    pub store: PgStore,
    pub metrics: Arc<Metrics>,
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/metrics", get(metrics))
        .route("/v1/chat/completions", post(chat))
        .route("/v1/responses", post(responses))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .with_state(state)
}

async fn metrics(State(state): State<ApiState>) -> Response {
    match state.metrics.render() {
        Ok(body) => (
            [(
                "content-type",
                "application/openmetrics-text; version=1.0.0; charset=utf-8",
            )],
            body,
        )
            .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        self.api_response(None)
    }
}

impl ProxyError {
    fn api_response(self, identity: Option<(uuid::Uuid, uuid::Uuid)>) -> Response {
        let status = match self.kind {
            ErrorKind::InvalidRequest => StatusCode::BAD_REQUEST,
            ErrorKind::Unauthorized => StatusCode::UNAUTHORIZED,
            ErrorKind::ModelNotFound => StatusCode::NOT_FOUND,
            ErrorKind::QuotaExceeded => StatusCode::TOO_MANY_REQUESTS,
            ErrorKind::Unavailable | ErrorKind::Persistence => StatusCode::SERVICE_UNAVAILABLE,
            ErrorKind::Timeout => StatusCode::GATEWAY_TIMEOUT,
            ErrorKind::Upstream | ErrorKind::UpstreamAuth | ErrorKind::Cancelled => {
                StatusCode::BAD_GATEWAY
            }
        };
        let body = json!({"error": {"message": self.message, "type": self.kind.as_str(), "code": self.kind.as_str(), "param": null}});
        match identity {
            Some((request_id, account_id)) => {
                tracing::warn!(event = "api_error", %request_id, %account_id, error_kind = self.kind.as_str(), body_json = %body)
            }
            None => {
                tracing::warn!(event = "api_error", error_kind = self.kind.as_str(), body_json = %body)
            }
        }
        (status, Json(body)).into_response()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireMessage {
    role: String,
    #[serde(default)]
    content: Value,
    tool_calls: Option<Vec<WireToolCall>>,
    tool_call_id: Option<String>,
    annotations: Option<Vec<ChatCitation>>,
    #[serde(default)]
    r#type: Option<String>,
    id: Option<String>,
    status: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatCitation {
    r#type: String,
    url_citation: CitationFields,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CitationFields {
    url: String,
    title: String,
    start_index: usize,
    end_index: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireToolCall {
    id: String,
    r#type: String,
    function: WireCallFunction,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireCallFunction {
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireFunction {
    name: String,
    description: Option<String>,
    parameters: Value,
    strict: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatRequest {
    model: String,
    messages: Vec<WireMessage>,
    #[serde(default)]
    stream: bool,
    stream_options: Option<StreamOptions>,
    max_tokens: Option<u32>,
    max_completion_tokens: Option<u32>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    stop: Option<Value>,
    n: Option<u32>,
    tools: Option<Vec<Value>>,
    tool_choice: Option<Value>,
    web_search_options: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponsesRequest {
    model: String,
    input: Value,
    instructions: Option<String>,
    #[serde(default)]
    stream: bool,
    max_output_tokens: Option<u32>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    store: Option<bool>,
    tools: Option<Vec<Value>>,
    tool_choice: Option<Value>,
}

fn invalid(message: &str) -> ProxyError {
    ProxyError::new(ErrorKind::InvalidRequest, message)
}

fn messages(input: Vec<WireMessage>, responses: bool) -> Result<Vec<Message>, ProxyError> {
    if input.is_empty() {
        return Err(invalid("At least one message is required"));
    }
    input
        .into_iter()
        .map(|message| {
            if message
                .r#type
                .as_deref()
                .is_some_and(|kind| !responses || kind != "message")
            {
                return Err(invalid("Unsupported message type"));
            }
            if !responses && (message.id.is_some() || message.status.is_some()) {
                return Err(invalid("Unsupported message fields"));
            }
            if message
                .status
                .as_deref()
                .is_some_and(|status| !matches!(status, "completed" | "incomplete" | "in_progress"))
            {
                return Err(invalid("Unsupported message status"));
            }
            let role = match message.role.as_str() {
                "system" => Role::System,
                "developer" => Role::Developer,
                "user" => Role::User,
                "assistant" => Role::Assistant,
                "tool" if !responses => Role::Tool,
                _ => return Err(invalid("Unsupported message role")),
            };
            if let Some(annotations) = message.annotations {
                if responses || !matches!(role, Role::Assistant) {
                    return Err(invalid("Annotations belong to assistant messages"));
                }
                for annotation in annotations {
                    let citation = annotation.url_citation;
                    let _ = citation.title;
                    if annotation.r#type != "url_citation"
                        || citation.url.is_empty()
                        || citation.start_index > citation.end_index
                    {
                        return Err(invalid("Invalid URL citation"));
                    }
                }
            }
            let mut tool_calls = Vec::new();
            for call in message.tool_calls.unwrap_or_default() {
                if responses || !matches!(role, Role::Assistant) || call.r#type != "function" {
                    return Err(invalid("Only assistant function tool calls are supported"));
                }
                validate_call(&call.id, &call.function.name, &call.function.arguments)?;
                tool_calls.push(ToolCall {
                    id: call.id,
                    name: call.function.name,
                    arguments: call.function.arguments,
                });
            }
            if matches!(role, Role::Tool) != message.tool_call_id.is_some() {
                return Err(invalid("tool_call_id is required only for tool messages"));
            }
            let content = match message.content {
                Value::Null if !tool_calls.is_empty() => String::new(),
                Value::String(text) => text,
                Value::Array(parts) => {
                    let mut text = String::new();
                    for part in parts {
                        let object = part
                            .as_object()
                            .ok_or_else(|| invalid("Expected a text content part"))?;
                        let kind = object.get("type").and_then(Value::as_str);
                        let output_text = responses
                            && matches!(role, Role::Assistant)
                            && kind == Some("output_text");
                        let supported = if responses {
                            kind == Some("input_text") || output_text
                        } else {
                            kind == Some("text")
                        };
                        if !supported
                            || object.keys().any(|key| {
                                key != "type"
                                    && key != "text"
                                    && !(output_text && key == "annotations")
                            })
                        {
                            return Err(invalid("Unsupported content part"));
                        }
                        if object
                            .get("annotations")
                            .is_some_and(|value| !value.is_array())
                        {
                            return Err(invalid("Invalid output text annotations"));
                        }
                        text.push_str(
                            object
                                .get("text")
                                .and_then(Value::as_str)
                                .ok_or_else(|| invalid("Expected text content"))?,
                        );
                    }
                    text
                }
                _ => return Err(invalid("Expected text content")),
            };
            Ok(Message {
                role,
                content,
                tool_calls,
                tool_call_id: message.tool_call_id,
            })
        })
        .collect()
}

fn validate(request: &InferenceRequest) -> Result<(), ProxyError> {
    if request.model.trim().is_empty() {
        return Err(invalid("model must not be empty"));
    }
    if request.max_output_tokens == Some(0) {
        return Err(invalid("Output token limit must be positive"));
    }
    if request
        .temperature
        .is_some_and(|value| !(0.0..=2.0).contains(&value))
    {
        return Err(invalid("temperature must be between 0 and 2"));
    }
    if request
        .top_p
        .is_some_and(|value| !(0.0..=1.0).contains(&value))
    {
        return Err(invalid("top_p must be between 0 and 1"));
    }
    let mut pending = std::collections::HashSet::new();
    let mut seen = std::collections::HashSet::new();
    for message in &request.messages {
        for call in &message.tool_calls {
            if !seen.insert(call.id.as_str()) {
                return Err(invalid("Duplicate tool call ID"));
            }
            pending.insert(call.id.as_str());
        }
        if let Some(id) = &message.tool_call_id
            && !pending.remove(id.as_str())
        {
            return Err(invalid(
                "Tool result must reference an earlier unanswered tool call",
            ));
        }
    }
    Ok(())
}

fn validate_call(id: &str, name: &str, arguments: &str) -> Result<(), ProxyError> {
    if id.is_empty()
        || name.is_empty()
        || !serde_json::from_str::<Value>(arguments).is_ok_and(|value| value.is_object())
    {
        return Err(invalid("Invalid function call"));
    }
    Ok(())
}

fn tools(
    values: Vec<Value>,
    choice: Option<Value>,
    responses: bool,
    web_search: Option<Value>,
) -> Result<(Vec<Tool>, ToolChoice), ProxyError> {
    let mut tools = Vec::new();
    let mut names = std::collections::HashSet::new();
    let mut search = false;
    for mut value in values {
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("Tool type is required"))?
            .to_owned();
        match kind.as_str() {
            "function" => {
                let function = if responses {
                    value
                        .as_object_mut()
                        .expect("tool has a type field")
                        .remove("type");
                    value
                } else {
                    let object = value.as_object_mut().expect("tool has a type field");
                    if object.len() != 2 {
                        return Err(invalid("Unsupported function tool fields"));
                    }
                    object
                        .remove("function")
                        .ok_or_else(|| invalid("Function definition is required"))?
                };
                let function: WireFunction = serde_json::from_value(function)
                    .map_err(|_| invalid("Invalid function definition"))?;
                if function.name.is_empty()
                    || function.name.len() > 64
                    || !function
                        .name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
                    || !function.parameters.is_object()
                    || !names.insert(function.name.clone())
                {
                    return Err(invalid("Invalid or duplicate function definition"));
                }
                tools.push(Tool::Function(FunctionDefinition {
                    name: function.name,
                    description: function.description,
                    parameters: function.parameters,
                    strict: function.strict,
                }));
            }
            "web_search" | "web_search_preview" if responses => {
                if value.as_object().expect("tool has a type field").len() != 1 || search {
                    return Err(invalid("Unsupported or duplicate web search options"));
                }
                tools.push(Tool::WebSearch);
                search = true;
            }
            _ => return Err(invalid("Unsupported tool type")),
        }
    }
    if let Some(options) = web_search {
        if !options.as_object().is_some_and(|object| object.is_empty()) || search {
            return Err(invalid("Only default web search options are supported"));
        }
        tools.push(Tool::WebSearch);
    }
    let choice = match choice {
        None => ToolChoice::Auto,
        Some(Value::String(value)) => match value.as_str() {
            "auto" => ToolChoice::Auto,
            "none" => ToolChoice::None,
            "required" => ToolChoice::Required,
            _ => return Err(invalid("Unsupported tool_choice")),
        },
        Some(value) if value.get("type").and_then(Value::as_str) == Some("function") => {
            let name = if responses {
                if value.as_object().expect("choice has a type field").len() != 2 {
                    return Err(invalid("Unsupported tool_choice fields"));
                }
                value.get("name").and_then(Value::as_str)
            } else {
                if value.as_object().expect("choice has a type field").len() != 2
                    || !value
                        .get("function")
                        .and_then(Value::as_object)
                        .is_some_and(|object| object.len() == 1)
                {
                    return Err(invalid("Unsupported tool_choice fields"));
                }
                value
                    .get("function")
                    .and_then(|value| value.get("name"))
                    .and_then(Value::as_str)
            }
            .ok_or_else(|| invalid("Function tool_choice requires a name"))?;
            if !names.contains(name) {
                return Err(invalid("tool_choice references an undefined function"));
            }
            ToolChoice::Function(name.to_owned())
        }
        _ => return Err(invalid("Unsupported tool_choice")),
    };
    if tools.is_empty() && matches!(choice, ToolChoice::Required) {
        return Err(invalid("Required tool_choice needs tools"));
    }
    Ok((tools, choice))
}

async fn authenticate(
    state: &ApiState,
    headers: &HeaderMap,
    peer: Option<IpAddr>,
) -> Result<Principal, ProxyError> {
    let _timer = state.metrics.time_stage(Stage::Authentication);
    if !headers.contains_key("authorization") {
        if let Some(ip) = peer.map(|ip| ip.to_canonical())
            && let Some(principal) = state.store.authenticate_ip(ip).await?
        {
            return Ok(principal);
        }
        return Err(ProxyError::new(
            ErrorKind::Unauthorized,
            "A bearer token is required",
        ));
    }
    let token = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, token)| scheme.eq_ignore_ascii_case("bearer") && !token.is_empty())
        .map(|(_, token)| token)
        .ok_or_else(|| ProxyError::new(ErrorKind::Unauthorized, "A bearer token is required"))?;
    let digest = Sha256::digest(token.as_bytes());
    state
        .store
        .authenticate_token(&digest)
        .await?
        .ok_or_else(|| ProxyError::new(ErrorKind::Unauthorized, "Invalid or revoked API token"))
}

async fn chat(
    State(state): State<ApiState>,
    headers: HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Response, ProxyError> {
    let started = Instant::now();
    let principal = authenticate(
        &state,
        &headers,
        peer.map(|Extension(ConnectInfo(addr))| addr.ip()),
    )
    .await?;
    let Json(request_body) = body.map_err(|_| invalid("Invalid Chat Completions JSON request"))?;
    let body = ChatRequest::deserialize(&request_body)
        .map_err(|_| invalid("Invalid or unsupported Chat Completions request"))?;
    if body.n.is_some_and(|n| n != 1) {
        return Err(invalid("Only n=1 is supported"));
    }
    if body.stream_options.is_some() && !body.stream {
        return Err(invalid("stream_options requires stream=true"));
    }
    if body.max_tokens.is_some() && body.max_completion_tokens.is_some() {
        return Err(invalid("Specify only one output token limit"));
    }
    let stop = match body.stop {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(text)) if !text.is_empty() => vec![text],
        Some(Value::Array(values)) if !values.is_empty() && values.len() <= 4 => values
            .into_iter()
            .map(|value| match value {
                Value::String(text) if !text.is_empty() => Ok(text),
                _ => Err(invalid("Invalid stop sequence")),
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(invalid("stop must contain one to four nonempty strings")),
    };
    let (tools, tool_choice) = tools(
        body.tools.unwrap_or_default(),
        body.tool_choice,
        false,
        body.web_search_options,
    )?;
    let request = InferenceRequest {
        tools,
        tool_choice,
        model: body.model.clone(),
        messages: messages(body.messages, false)?,
        max_output_tokens: body.max_completion_tokens.or(body.max_tokens),
        temperature: body.temperature,
        top_p: body.top_p,
        stop,
    };
    validate(&request)?;
    let account_id = principal.account_id;
    let stream = state.gateway.execute(principal, request, started).await?;
    let request_id = stream.request_id;
    tracing::info!(event = "api_request", %request_id, %account_id, body_json = %request_body);
    let result = if body.stream {
        chat_stream(
            stream,
            account_id,
            body.model,
            body.stream_options
                .is_some_and(|options| options.include_usage),
        )
        .await
    } else {
        collect(stream, account_id, body.model, false).await
    };
    Ok(result.unwrap_or_else(|error| error.api_response(Some((request_id, account_id)))))
}

async fn responses(
    State(state): State<ApiState>,
    headers: HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Response, ProxyError> {
    let started = Instant::now();
    let principal = authenticate(
        &state,
        &headers,
        peer.map(|Extension(ConnectInfo(addr))| addr.ip()),
    )
    .await?;
    let Json(request_body) = body.map_err(|_| invalid("Invalid Responses JSON request"))?;
    let body = ResponsesRequest::deserialize(&request_body)
        .map_err(|_| invalid("Invalid or unsupported Responses request"))?;
    if body.store == Some(true) {
        return Err(invalid("Stored responses are not supported"));
    }
    let mut input = match body.input {
        Value::String(text) => vec![Message {
            role: Role::User,
            content: text,
            tool_calls: Vec::new(),
            tool_call_id: None,
        }],
        Value::Array(items) => response_messages(items)?,
        _ => return Err(invalid("input must be text or an array of text messages")),
    };
    if let Some(instructions) = body.instructions {
        input.insert(
            0,
            Message {
                role: Role::System,
                content: instructions,
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
        );
    }
    let (tools, tool_choice) = tools(body.tools.unwrap_or_default(), body.tool_choice, true, None)?;
    let request = InferenceRequest {
        tools,
        tool_choice,
        model: body.model.clone(),
        messages: input,
        max_output_tokens: body.max_output_tokens,
        temperature: body.temperature,
        top_p: body.top_p,
        stop: Vec::new(),
    };
    validate(&request)?;
    let account_id = principal.account_id;
    let stream = state.gateway.execute(principal, request, started).await?;
    let request_id = stream.request_id;
    tracing::info!(event = "api_request", %request_id, %account_id, body_json = %request_body);
    let result = if body.stream {
        responses_stream(stream, account_id, body.model).await
    } else {
        collect(stream, account_id, body.model, true).await
    };
    Ok(result.unwrap_or_else(|error| error.api_response(Some((request_id, account_id)))))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputCall {
    r#type: String,
    call_id: String,
    name: String,
    arguments: String,
    id: Option<String>,
    status: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputResult {
    r#type: String,
    call_id: String,
    output: String,
    id: Option<String>,
    status: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchHistory {
    r#type: String,
    id: String,
    status: String,
    action: Option<SearchAction>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum SearchAction {
    Search { query: String },
}

fn response_messages(items: Vec<Value>) -> Result<Vec<Message>, ProxyError> {
    if items.is_empty() {
        return Err(invalid("At least one input item is required"));
    }
    let mut result = Vec::new();
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("web_search_call") => {
                let search: SearchHistory = serde_json::from_value(item)
                    .map_err(|_| invalid("Unsupported web search history item"))?;
                let _ = search.r#type;
                if search.id.is_empty() || search.status != "completed" {
                    return Err(invalid("Only completed web search history is supported"));
                }
                if let Some(SearchAction::Search { query }) = search.action {
                    let _ = query;
                }
                // Completed search metadata is informational; the following assistant text is the result.
            }
            Some("function_call") => {
                let call: InputCall = serde_json::from_value(item)
                    .map_err(|_| invalid("Invalid function_call item"))?;
                let _ = (call.r#type, call.id, call.status);
                validate_call(&call.call_id, &call.name, &call.arguments)?;
                result.push(Message {
                    role: Role::Assistant,
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: call.call_id,
                        name: call.name,
                        arguments: call.arguments,
                    }],
                    tool_call_id: None,
                });
            }
            Some("function_call_output") => {
                let output: InputResult = serde_json::from_value(item)
                    .map_err(|_| invalid("Invalid function_call_output item"))?;
                let _ = (output.r#type, output.id, output.status);
                result.push(Message {
                    role: Role::Tool,
                    content: output.output,
                    tool_calls: Vec::new(),
                    tool_call_id: Some(output.call_id),
                });
            }
            _ => result.extend(messages(
                vec![serde_json::from_value(item).map_err(|_| invalid("Unsupported input item"))?],
                true,
            )?),
        }
    }
    if result.is_empty() {
        return Err(invalid("At least one message or function item is required"));
    }
    Ok(result)
}
