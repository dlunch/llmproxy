use std::{collections::HashMap, convert::Infallible};

use axum::{
    Json,
    response::{IntoResponse, Response, Sse, sse::Event},
};
use futures_util::StreamExt;
use serde_json::{Value, json};

use crate::{
    core::{ErrorKind, FinishReason, ProviderEvent, ProxyError, Usage, WebSearchStatus},
    gateway::GatewayStream,
};

enum Item {
    Message {
        text: String,
        annotations: Vec<Value>,
    },
    Function {
        id: String,
        name: String,
        arguments: String,
    },
    Search {
        id: String,
        query: Option<String>,
        status: &'static str,
    },
}

struct Output {
    id: String,
    account_id: uuid::Uuid,
    created: i64,
    model: String,
    items: Vec<Item>,
    message: Option<usize>,
    calls: HashMap<usize, usize>,
    searches: HashMap<String, usize>,
    usage: Option<Usage>,
}

impl Output {
    fn new(stream: &GatewayStream, account_id: uuid::Uuid, model: String) -> Self {
        Self {
            id: stream.request_id.to_string(),
            account_id,
            created: stream.created_at.timestamp(),
            model,
            items: Vec::new(),
            message: None,
            calls: HashMap::new(),
            searches: HashMap::new(),
            usage: None,
        }
    }

    fn sse(&self, body: String, name: Option<&str>) -> Result<Event, Infallible> {
        tracing::info!(event = "api_response_chunk", request_id = %self.id, account_id = %self.account_id, body_json = %body);
        let mut event = Event::default();
        if let Some(name) = name {
            event = event.event(name);
        }
        Ok(event.data(body))
    }

    fn item(&self, index: usize, status: &str) -> Value {
        match &self.items[index] {
            Item::Message { text, annotations } => {
                json!({"id": format!("msg_{}_{}", self.id, index), "type":"message", "role":"assistant", "status":status,
                "content":[{"type":"output_text","text":text,"annotations":annotations}]})
            }
            Item::Function {
                id,
                name,
                arguments,
            } => {
                json!({"id":format!("fc_{}_{}",self.id,index),"type":"function_call","call_id":id,"name":name,"arguments":arguments,"status":status})
            }
            Item::Search { id, query, status } => {
                let mut value = json!({"id":id,"type":"web_search_call","status":status});
                if let Some(query) = query {
                    value["action"] = json!({"type":"search","query":query});
                }
                value
            }
        }
    }

    fn response(&self, status: &str) -> Value {
        let item_status = match status {
            "in_progress" => "in_progress",
            "completed" => "completed",
            _ => "incomplete",
        };
        let output: Vec<_> = self
            .items
            .iter()
            .enumerate()
            .map(|(index, _)| self.item(index, item_status))
            .collect();
        json!({"id":format!("resp_{}",self.id),"object":"response","created_at":self.created,"model":self.model,
            "status":status,"error":null,"incomplete_details":if status == "incomplete" {json!({"reason":"max_output_tokens"})} else {Value::Null},
            "output":output,"usage":usage_value(self.usage.as_ref(),true),"store":false})
    }

    fn chat_message(&self) -> Value {
        let mut text = String::new();
        let mut annotations = Vec::new();
        let mut calls = Vec::new();
        for item in &self.items {
            match item {
                Item::Message { text: content, annotations: citations } => {
                    text.push_str(content);
                    annotations.extend(citations.iter().map(|citation| json!({"type":"url_citation","url_citation":{
                        "url":citation["url"],"title":citation["title"],"start_index":citation["start_index"],"end_index":citation["end_index"]
                    }})));
                }
                Item::Function { id, name, arguments } => calls.push(json!({"id":id,"type":"function","function":{"name":name,"arguments":arguments}})),
                Item::Search { .. } => {}
            }
        }
        let mut message = json!({"role":"assistant","content":if text.is_empty() && !calls.is_empty() {Value::Null} else {json!(text)}});
        if !calls.is_empty() {
            message["tool_calls"] = json!(calls);
        }
        if !annotations.is_empty() {
            message["annotations"] = json!(annotations);
        }
        message
    }

    // Returns Responses events; collected responses reuse the same normalized output assembly.
    fn apply(&mut self, event: ProviderEvent) -> Result<Vec<Value>, ProxyError> {
        let mut events = Vec::new();
        match event {
            ProviderEvent::TextDelta(_) | ProviderEvent::Citation { .. }
                if self.message.is_none() =>
            {
                let index = self.items.len();
                self.items.push(Item::Message {
                    text: String::new(),
                    annotations: Vec::new(),
                });
                self.message = Some(index);
                let mut item = self.item(index, "in_progress");
                item["content"] = json!([]);
                events.push(
                    json!({"type":"response.output_item.added","output_index":index,"item":item}),
                );
                events.push(json!({"type":"response.content_part.added","output_index":index,"item_id":format!("msg_{}_{}",self.id,index),"content_index":0,
                    "part":{"type":"output_text","text":"","annotations":[]}}));
                events.extend(self.apply(event)?);
            }
            ProviderEvent::TextDelta(delta) => {
                let index = self.message.expect("text message was initialized");
                let Item::Message { text, .. } = &mut self.items[index] else {
                    unreachable!()
                };
                text.push_str(&delta);
                events.push(json!({"type":"response.output_text.delta","output_index":index,"item_id":format!("msg_{}_{}",self.id,index),"content_index":0,"delta":delta,"logprobs":[]}));
            }
            ProviderEvent::Citation {
                url,
                title,
                start_index,
                end_index,
            } => {
                let index = self.message.expect("text message was initialized");
                let Item::Message { annotations, .. } = &mut self.items[index] else {
                    unreachable!()
                };
                let citation = json!({"type":"url_citation","url":url,"title":title,"start_index":start_index,"end_index":end_index});
                events.push(json!({"type":"response.output_text.annotation.added","output_index":index,"item_id":format!("msg_{}_{}",self.id,index),"content_index":0,"annotation_index":annotations.len(),"annotation":citation}));
                annotations.push(citation);
            }
            ProviderEvent::ToolCallDelta {
                index: call_index,
                id,
                name,
                arguments,
            } => {
                let index = if let Some(index) = self.calls.get(&call_index) {
                    *index
                } else {
                    let id = id.as_ref().filter(|id| !id.is_empty()).ok_or_else(|| {
                        ProxyError::new(ErrorKind::Upstream, "Function call started without an ID")
                    })?;
                    let name = name
                        .as_ref()
                        .filter(|name| !name.is_empty())
                        .ok_or_else(|| {
                            ProxyError::new(
                                ErrorKind::Upstream,
                                "Function call started without a name",
                            )
                        })?;
                    let index = self.items.len();
                    self.items.push(Item::Function {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: String::new(),
                    });
                    self.calls.insert(call_index, index);
                    events.push(json!({"type":"response.output_item.added","output_index":index,"item":self.item(index,"in_progress")}));
                    index
                };
                let Item::Function {
                    id: previous_id,
                    name: previous_name,
                    arguments: value,
                } = &mut self.items[index]
                else {
                    unreachable!()
                };
                if id.is_some_and(|id| id != *previous_id)
                    || name.is_some_and(|name| name != *previous_name)
                {
                    return Err(ProxyError::new(
                        ErrorKind::Upstream,
                        "Function call identity changed during streaming",
                    ));
                }
                value.push_str(&arguments);
                if !arguments.is_empty() {
                    events.push(json!({"type":"response.function_call_arguments.delta","output_index":index,"item_id":format!("fc_{}_{}",self.id,index),"delta":arguments}));
                }
            }
            ProviderEvent::WebSearch { id, query, status } => {
                let index = if let Some(index) = self.searches.get(&id) {
                    *index
                } else {
                    let index = self.items.len();
                    self.items.push(Item::Search {
                        id: id.clone(),
                        query: query.clone(),
                        status: "in_progress",
                    });
                    self.searches.insert(id.clone(), index);
                    events.push(json!({"type":"response.output_item.added","output_index":index,"item":self.item(index,"in_progress")}));
                    index
                };
                let status = match status {
                    WebSearchStatus::InProgress => "in_progress",
                    WebSearchStatus::Searching => "searching",
                    WebSearchStatus::Completed => "completed",
                };
                let Item::Search {
                    query: previous_query,
                    status: previous_status,
                    ..
                } = &mut self.items[index]
                else {
                    unreachable!()
                };
                *previous_status = status;
                if query.is_some() {
                    *previous_query = query;
                }
                events.push(json!({"type":format!("response.web_search_call.{status}"),"output_index":index,"item_id":id}));
            }
            ProviderEvent::Usage(usage) => self.usage = Some(usage),
            ProviderEvent::Finished(_) => {
                unreachable!("terminal events are handled by the response serializer")
            }
        }
        Ok(events)
    }

    fn completed_events(&self, reason: &FinishReason) -> Vec<Value> {
        let status = if matches!(reason, FinishReason::Length) {
            "incomplete"
        } else {
            "completed"
        };
        let mut events = Vec::new();
        for (index, item) in self.items.iter().enumerate() {
            match item {
                Item::Message { text, .. } => {
                    events.push(json!({"type":"response.output_text.done","output_index":index,"item_id":format!("msg_{}_{}",self.id,index),"content_index":0,"text":text,"logprobs":[]}));
                    events.push(json!({"type":"response.content_part.done","output_index":index,"item_id":format!("msg_{}_{}",self.id,index),"content_index":0,"part":self.item(index,status)["content"][0]}));
                }
                Item::Function { name, arguments, .. } => events.push(json!({"type":"response.function_call_arguments.done","output_index":index,"item_id":format!("fc_{}_{}",self.id,index),"name":name,"arguments":arguments})),
                Item::Search { .. } => {}
            }
            events.push(json!({"type":"response.output_item.done","output_index":index,"item":self.item(index,status)}));
        }
        events.push(json!({"type":format!("response.{status}"),"response":self.response(status)}));
        events
    }
}

fn usage_value(usage: Option<&Usage>, responses: bool) -> Value {
    match usage {
        Some(usage) if responses => {
            json!({"input_tokens":usage.input_tokens,"output_tokens":usage.output_tokens,"total_tokens":usage.input_tokens.saturating_add(usage.output_tokens)})
        }
        Some(usage) => {
            json!({"prompt_tokens":usage.input_tokens,"completion_tokens":usage.output_tokens,"total_tokens":usage.input_tokens.saturating_add(usage.output_tokens)})
        }
        None => Value::Null,
    }
}

pub(super) async fn collect(
    mut stream: GatewayStream,
    account_id: uuid::Uuid,
    model: String,
    responses: bool,
) -> Result<Response, ProxyError> {
    let mut output = Output::new(&stream, account_id, model);
    while let Some(event) = stream.next().await {
        match event? {
            ProviderEvent::Finished(reason) => {
                let response = if responses {
                    output.response(if matches!(reason, FinishReason::Length) {
                        "incomplete"
                    } else {
                        "completed"
                    })
                } else {
                    let finish = match reason {
                        FinishReason::Stop => "stop",
                        FinishReason::Length => "length",
                        FinishReason::ToolCalls => "tool_calls",
                    };
                    let mut value = json!({"id":format!("chatcmpl_{}",output.id),"object":"chat.completion","created":output.created,"model":output.model,
                        "choices":[{"index":0,"message":output.chat_message(),"finish_reason":finish}]});
                    if output.usage.is_some() {
                        value["usage"] = usage_value(output.usage.as_ref(), false);
                    }
                    value
                };
                tracing::info!(event = "api_response", request_id = %output.id, account_id = %output.account_id, body_json = %response);
                return Ok(Json(response).into_response());
            }
            event => {
                output.apply(event)?;
            }
        }
    }
    Err(ProxyError::new(
        ErrorKind::Upstream,
        "Provider stream ended without completion",
    ))
}

pub(super) async fn chat_stream(
    mut stream: GatewayStream,
    account_id: uuid::Uuid,
    model: String,
    include_usage: bool,
) -> Result<Response, ProxyError> {
    let mut output = Output::new(&stream, account_id, model);
    let first = stream.next().await.ok_or_else(|| {
        ProxyError::new(
            ErrorKind::Upstream,
            "Provider stream ended without completion",
        )
    })??;
    let mut stream = futures_util::stream::iter([Ok(first)]).chain(stream);
    let body = async_stream::stream! {
        let mut initial = json!({"id":format!("chatcmpl_{}",output.id),"object":"chat.completion.chunk","created":output.created,"model":output.model,
            "choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]});
        if include_usage { initial["usage"] = Value::Null; }
        yield output.sse(initial.to_string(),None);
        while let Some(event) = stream.next().await {
            let event = match event {
                Ok(event) => event,
                Err(error) => {
                    yield output.sse(json!({"error":{"message":error.message,"type":error.kind.as_str(),"code":error.kind.as_str()}}).to_string(),None);
                    return;
                }
            };
            let (delta,finish) = match &event {
                ProviderEvent::TextDelta(text) => (json!({"content":text}),Value::Null),
                ProviderEvent::ToolCallDelta { index,id,name,arguments } => {
                    let mut call = json!({"index":index,"function":{"arguments":arguments}});
                    if let Some(id) = id { call["id"] = json!(id); call["type"] = json!("function"); }
                    if let Some(name) = name { call["function"]["name"] = json!(name); }
                    (json!({"tool_calls":[call]}),Value::Null)
                }
                ProviderEvent::Citation { url,title,start_index,end_index } => (json!({"annotations":[{"type":"url_citation","url_citation":{"url":url,"title":title,"start_index":start_index,"end_index":end_index}}]}),Value::Null),
                ProviderEvent::Finished(reason) => (json!({}),json!(match reason { FinishReason::Stop => "stop", FinishReason::Length => "length", FinishReason::ToolCalls => "tool_calls" })),
                ProviderEvent::Usage(_) | ProviderEvent::WebSearch { .. } => {
                    if let Err(error) = output.apply(event) {
                        yield output.sse(json!({"error":{"message":error.message,"type":error.kind.as_str()}}).to_string(),None);
                        return;
                    }
                    continue;
                }
            };
            let terminal = !finish.is_null();
            if !terminal {
                match output.apply(event) {
                    Ok(_) => {},
                    Err(error) => {
                        yield output.sse(json!({"error":{"message":error.message,"type":error.kind.as_str()}}).to_string(),None);
                        return;
                    }
                }
            }
            let mut chunk = json!({"id":format!("chatcmpl_{}",output.id),"object":"chat.completion.chunk","created":output.created,"model":output.model,
                "choices":[{"index":0,"delta":delta,"finish_reason":finish}]});
            if include_usage { chunk["usage"] = Value::Null; }
            yield output.sse(chunk.to_string(),None);
            if terminal {
                if include_usage && output.usage.is_some() {
                    yield output.sse(json!({"id":format!("chatcmpl_{}",output.id),"object":"chat.completion.chunk","created":output.created,"model":output.model,"choices":[],"usage":usage_value(output.usage.as_ref(),false)}).to_string(),None);
                }
                yield output.sse("[DONE]".into(),None);
                return;
            }
        }
        yield output.sse(json!({"error":{"message":"Provider stream ended without completion","type":"upstream","code":"upstream"}}).to_string(),None);
    };
    Ok(Sse::new(body).into_response())
}

fn response_event(
    mut value: Value,
    sequence: &mut u64,
    output: &Output,
) -> Result<Event, Infallible> {
    value["sequence_number"] = json!(*sequence);
    *sequence += 1;
    output.sse(value.to_string(), value["type"].as_str())
}

pub(super) async fn responses_stream(
    mut stream: GatewayStream,
    account_id: uuid::Uuid,
    model: String,
) -> Result<Response, ProxyError> {
    let mut output = Output::new(&stream, account_id, model);
    let first = stream.next().await.ok_or_else(|| {
        ProxyError::new(
            ErrorKind::Upstream,
            "Provider stream ended without completion",
        )
    })??;
    let mut stream = futures_util::stream::iter([Ok(first)]).chain(stream);
    let body = async_stream::stream! {
        let mut sequence = 0;
        yield response_event(json!({"type":"response.created","response":output.response("in_progress")}),&mut sequence,&output);
        yield response_event(json!({"type":"response.in_progress","response":output.response("in_progress")}),&mut sequence,&output);
        while let Some(event) = stream.next().await {
            let events = match event {
                Ok(ProviderEvent::Finished(reason)) => {
                    for event in output.completed_events(&reason) { yield response_event(event,&mut sequence,&output); }
                    return;
                }
                Ok(event) => output.apply(event),
                Err(error) => Err(error),
            };
            match events {
                Ok(events) => for event in events { yield response_event(event,&mut sequence,&output); },
                Err(error) => {
                    let mut response = output.response("failed");
                    response["error"] = json!({"code":error.kind.as_str(),"message":error.message});
                    yield response_event(json!({"type":"response.failed","response":response}),&mut sequence,&output);
                    return;
                }
            }
        }
        let mut response = output.response("failed");
        response["error"] = json!({"code":"upstream","message":"Provider stream ended without completion"});
        yield response_event(json!({"type":"response.failed","response":response}),&mut sequence,&output);
    };
    Ok(Sse::new(body).into_response())
}
