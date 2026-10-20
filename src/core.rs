use std::pin::Pin;

use async_trait::async_trait;
use futures_util::Stream;
use tokio_util::sync::CancellationToken;

pub struct Principal {
    pub account_id: uuid::Uuid,
    pub credential: Credential,
}

pub enum Credential {
    ApiToken(uuid::Uuid),
    AllowedIp {
        range_id: uuid::Uuid,
        address: std::net::IpAddr,
    },
}

pub struct AccountUsage {
    pub account_id: uuid::Uuid,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

pub struct Message {
    pub role: Role,
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub tool_call_id: Option<String>,
}

pub enum Role {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}

pub struct InferenceRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub max_output_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub stop: Vec<String>,
    pub tools: Vec<Tool>,
    pub tool_choice: ToolChoice,
}

pub struct FunctionDefinition {
    pub name: String,
    pub description: Option<String>,
    pub parameters: serde_json::Value,
    pub strict: Option<bool>,
}

pub enum Tool {
    Function(FunctionDefinition),
    WebSearch,
}

pub enum ToolChoice {
    Auto,
    None,
    Required,
    Function(String),
}

pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

pub enum WebSearchStatus {
    InProgress,
    Searching,
    Completed,
}

pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
}

pub enum ProviderEvent {
    TextDelta(String),
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    },
    WebSearch {
        id: String,
        query: Option<String>,
        status: WebSearchStatus,
    },
    Citation {
        url: String,
        title: String,
        start_index: usize,
        end_index: usize,
    },
    Usage(Usage),
    Finished(FinishReason),
}

pub type EventStream = Pin<Box<dyn Stream<Item = Result<ProviderEvent, ProxyError>> + Send>>;

#[async_trait]
pub trait Provider: Send + Sync {
    async fn infer(
        &self,
        request: InferenceRequest,
        cancellation: CancellationToken,
    ) -> Result<EventStream, ProxyError>;

    async fn shutdown(&self) {}
}

#[derive(Debug, Clone, Copy)]
pub enum ErrorKind {
    InvalidRequest,
    Unauthorized,
    ModelNotFound,
    QuotaExceeded,
    Unavailable,
    UpstreamAuth,
    Upstream,
    Timeout,
    Cancelled,
    Persistence,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::Unauthorized => "unauthorized",
            Self::ModelNotFound => "model_not_found",
            Self::QuotaExceeded => "quota_exceeded",
            Self::Unavailable => "unavailable",
            Self::UpstreamAuth => "upstream_auth",
            Self::Upstream => "upstream",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Persistence => "persistence",
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ProxyError {
    pub kind: ErrorKind,
    pub message: String,
}

impl ProxyError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

pub struct Route {
    pub model: String,
    pub provider_id: String,
    pub upstream_model: String,
}
