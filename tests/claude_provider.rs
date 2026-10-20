use std::{os::unix::fs::PermissionsExt, path::Path, time::Duration};

use futures_util::StreamExt;
use llmproxy::{
    core::{
        ErrorKind, FinishReason, FunctionDefinition, InferenceRequest, Message, Provider,
        ProviderEvent, Role, Tool, ToolCall, ToolChoice, WebSearchStatus,
    },
    providers::claude::ClaudeProvider,
    store::PgStore,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

const FIXTURE: &str = r#"#!/usr/bin/env python3
import json,os,pathlib,sys,time
args=sys.argv[1:]
def arg(name): return args[args.index(name)+1]
def emit(value): print(json.dumps(value),flush=True)
def read(): return json.loads(sys.stdin.readline())
def stream(event): emit({'type':'stream_event','event':event})
def assistant(mid,usage,block): emit({'type':'assistant','message':{'id':mid,'usage':usage,'content':[block]}})
config=pathlib.Path(os.environ['CLAUDE_CONFIG_DIR'])
capture=pathlib.Path(__file__).parent
credentials=json.loads((config/'.credentials.json').read_text())
assert json.loads((config/'.claude.json').read_text())['oauthAccount']['accountUuid']=='fixture-account'
rotation=credentials['rotation']+1
(config/'.credentials.json').write_text(json.dumps({'rotation':rotation}))
# Only login state is persisted; arbitrary CLI state is discarded.
(config/'.claude.json').write_text(json.dumps({'hasCompletedOnboarding':True,'projects':{'private':'discard'}}))
(capture/'pid').write_text(str(os.getpid()))
assert '--safe-mode' in args and '--no-session-persistence' in args
assert '--strict-mcp-config' in args and '--disable-slash-commands' in args
assert arg('--setting-sources')=='' and arg('--permission-mode')=='dontAsk'
assert arg('--permission-prompts')=='none'
assert arg('--tools') in ['', 'WebSearch']
assert pathlib.Path.cwd()!=config
init=read()
assert init['request']['subtype']=='initialize'
emit({'type':'control_response','response':{'subtype':'success','request_id':init['request_id'],'response':{}}})
resuming=os.environ.get('CLAUDE_CODE_RESUME_INTERRUPTED_TURN')=='1'
user=None if resuming else read()
assert user is None or user['type']=='user'
functions=[]
if json.loads(arg('--mcp-config'))['mcpServers']:
 for method in ['initialize','tools/list']:
  emit({'type':'control_request','request_id':method,'request':{'subtype':'mcp_message','server_name':'client','message':{'jsonrpc':'2.0','id':method,'method':method}}})
  reply=read()
  assert reply['type']=='control_response'
  if method=='tools/list': functions=reply['response']['response']['mcp_response']['result']['tools']
history=[json.loads(line) for line in pathlib.Path(arg('--resume')).read_text().splitlines()] if '--resume' in args else []
(capture/'capture.json').write_text(json.dumps({'rotation':rotation,'config_dir':str(config),'history':history,'user':user,'functions':functions,'tools':arg('--tools'),'system':pathlib.Path(arg('--system-prompt-file')).read_text()}))
scenario=arg('--model')
if scenario=='malformed':
 print('{broken',flush=True)
 sys.exit(0)
if scenario=='parallel':
 assert arg('--tools')=='WebSearch'
 first={'input_tokens':10,'cache_read_input_tokens':2,'output_tokens':0}
 stream({'type':'message_start','message':{'id':'msg-search','usage':first}})
 assistant('msg-search',first,{'type':'tool_use','id':'search-1','name':'WebSearch','input':{'query':'forecast'}})
 stream({'type':'message_delta','usage':{'output_tokens':5}})
 stream({'type':'message_stop'})
 emit({'type':'user','message':{'content':[{'type':'tool_result','tool_use_id':'search-1','content':'sunny'}]}})
 second={'input_tokens':20,'cache_creation_input_tokens':3,'output_tokens':0}
 stream({'type':'message_start','message':{'id':'msg-tools','usage':second}})
 for index,city in enumerate(['Seoul','Busan']):
  call={'type':'tool_use','id':f'call-{index}','name':'mcp__client__lookup_weather','input':{'city':city}}
  assistant('msg-tools',second,call)
  if index==0:
   emit({'type':'control_request','request_id':'must-not-execute','request':{'subtype':'mcp_message','server_name':'client','message':{'jsonrpc':'2.0','id':99,'method':'tools/call','params':{'name':'lookup_weather','arguments':{'city':city}}}}})
 stream({'type':'message_delta','usage':{'output_tokens':7}})
 stream({'type':'message_stop'})
 # A handoff kills this process instead of replying to tools/call or executing it.
 response=sys.stdin.readline()
 if response: (capture/'unexpected-tool-response').write_text(response)
 time.sleep(60)
else:
 stream({'type':'content_block_delta','delta':{'type':'text_delta','text':'hello'}})
 if scenario=='cancel': time.sleep(60)
 if scenario=='pause':
  while not (capture/'release').exists(): time.sleep(0.01)
 if scenario=='truncated': sys.exit(0)
 emit({'type':'result','subtype':'success','is_error':False,'stop_reason':'end_turn','usage':{'input_tokens':2,'cache_read_input_tokens':3,'cache_creation_input_tokens':1,'output_tokens':4}})
 sys.exit(7 if scenario=='nonzero' else 0)
"#;

async fn fixture() -> (tempfile::TempDir, ClaudeProvider, sqlx::PgPool) {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("claude");
    std::fs::write(&script, FIXTURE).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let pool = auth_database().await;
    let provider = ClaudeProvider::new("claude-main".into(), script, PgStore::new(pool.clone()));
    (dir, provider, pool)
}

async fn auth_database() -> sqlx::PgPool {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
    let admin = sqlx::PgPool::connect(&url).await.unwrap();
    let schema = format!("claude_test_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let options = url
        .parse::<sqlx::postgres::PgConnectOptions>()
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = sqlx::PgPool::connect_with(options).await.unwrap();
    PgStore::new(pool.clone()).migrate().await.unwrap();
    let account = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO accounts(id,label) VALUES ($1,'Claude fixture')")
        .bind(account)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO provider_instances(id,account_id) VALUES ('claude-main',$1)")
        .bind(account)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO claude_credentials(provider_id,credentials,oauth_account,user_id) VALUES ('claude-main',$1,$2,'fixture-user')")
        .bind(json!({"rotation":0})).bind(json!({"accountUuid":"fixture-account"})).execute(&pool).await.unwrap();
    pool
}

fn request(model: &str) -> InferenceRequest {
    InferenceRequest {
        model: model.into(),
        messages: vec![Message {
            role: Role::User,
            content: "hello".into(),
            tool_calls: vec![],
            tool_call_id: None,
        }],
        max_output_tokens: None,
        temperature: None,
        top_p: None,
        stop: vec![],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
    }
}

fn function() -> Tool {
    Tool::Function(FunctionDefinition {
        name: "lookup_weather".into(),
        description: Some("Look up weather".into()),
        parameters: json!({"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}),
        strict: None,
    })
}

async fn assert_process_terminated(directory: &Path) {
    let pid = std::fs::read_to_string(directory.join("pid")).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match std::fs::read_to_string(format!("/proc/{pid}/status")) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Ok(status) if status.contains("\nState:\tZ") => break,
                Ok(_) => tokio::time::sleep(Duration::from_millis(10)).await,
                Err(error) => panic!("Cannot inspect subprocess: {error}"),
            }
        }
    })
    .await
    .expect("Claude process survived cancellation or handoff");
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL PostgreSQL"]
async fn subprocess_requires_successful_result_and_exit() {
    for scenario in ["success", "nonzero", "malformed", "truncated"] {
        let (_dir, provider, _pool) = fixture().await;
        let mut stream = provider
            .infer(request(scenario), CancellationToken::new())
            .await
            .unwrap();
        let mut text = String::new();
        let mut finished = false;
        let mut failed = false;
        let mut usage = None;
        while let Some(event) = stream.next().await {
            match event {
                Ok(ProviderEvent::TextDelta(delta)) => text.push_str(&delta),
                Ok(ProviderEvent::Usage(value)) => {
                    usage = Some((value.input_tokens, value.output_tokens))
                }
                Ok(ProviderEvent::Finished(FinishReason::Stop)) => finished = true,
                Err(error) => {
                    assert!(matches!(error.kind, ErrorKind::Upstream));
                    failed = true;
                }
                _ => panic!("unexpected provider event"),
            }
        }
        if scenario == "success" {
            assert_eq!(text, "hello");
            assert!(finished);
            assert!(!failed);
            assert_eq!(usage, Some((6, 4)));
        } else {
            assert!(!finished, "{scenario} incorrectly completed");
            assert!(failed, "{scenario} did not fail");
        }
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL PostgreSQL"]
async fn parallel_function_handoff_preserves_calls_and_cumulative_search_usage() {
    let (dir, provider, pool) = fixture().await;
    let mut input = request("parallel");
    input.tools = vec![Tool::WebSearch, function()];
    let mut stream = provider
        .infer(input, CancellationToken::new())
        .await
        .unwrap();
    let mut calls = vec![];
    let mut usage = None;
    let mut search_started = false;
    let mut search_completed = false;
    let mut finished = false;
    while let Some(event) = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .unwrap()
    {
        match event.unwrap() {
            ProviderEvent::ToolCallDelta {
                index,
                id,
                name,
                arguments,
            } => calls.push((
                index,
                id.unwrap(),
                name.unwrap(),
                serde_json::from_str::<Value>(&arguments).unwrap(),
            )),
            ProviderEvent::Usage(value) => usage = Some((value.input_tokens, value.output_tokens)),
            ProviderEvent::WebSearch {
                id,
                query,
                status: WebSearchStatus::Searching,
            } => {
                assert_eq!(id, "search-1");
                assert_eq!(query.as_deref(), Some("forecast"));
                search_started = true;
            }
            ProviderEvent::WebSearch {
                id,
                status: WebSearchStatus::Completed,
                ..
            } => {
                assert_eq!(id, "search-1");
                search_completed = true;
            }
            ProviderEvent::Finished(FinishReason::ToolCalls) => finished = true,
            _ => panic!("unexpected event"),
        }
    }
    assert_eq!(
        calls,
        vec![
            (
                0,
                "call-0".into(),
                "lookup_weather".into(),
                json!({"city":"Seoul"})
            ),
            (
                1,
                "call-1".into(),
                "lookup_weather".into(),
                json!({"city":"Busan"})
            )
        ]
    );
    assert_eq!(usage, Some((35, 12)));
    assert!(search_started && search_completed && finished);
    let capture: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("capture.json")).unwrap()).unwrap();
    assert_eq!(capture["functions"][0]["name"], "lookup_weather");
    assert_eq!(
        capture["functions"][0]["inputSchema"]["required"],
        json!(["city"])
    );
    assert!(!dir.path().join("unexpected-tool-response").exists());
    assert_process_terminated(dir.path()).await;
    let saved = PgStore::new(pool)
        .claude_credentials("claude-main")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.credentials["rotation"], 1);
    assert_eq!(
        saved.oauth_account.unwrap()["accountUuid"],
        "fixture-account"
    );
    assert_eq!(saved.user_id.as_deref(), Some("fixture-user"));
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL PostgreSQL"]
async fn history_keeps_function_calls_and_client_results_in_separate_roles() {
    let (dir, provider, _pool) = fixture().await;
    let mut input = request("roundtrip");
    input.tools = vec![function()];
    input.messages.insert(
        0,
        Message {
            role: Role::System,
            content: "Use the supplied weather data.".into(),
            tool_calls: vec![],
            tool_call_id: None,
        },
    );
    input.messages.push(Message {
        role: Role::Assistant,
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: "call-prior".into(),
            name: "lookup_weather".into(),
            arguments: r#"{"city":"Seoul"}"#.into(),
        }],
        tool_call_id: None,
    });
    input.messages.push(Message {
        role: Role::Tool,
        content: "sunny".into(),
        tool_calls: vec![],
        tool_call_id: Some("call-prior".into()),
    });
    let mut stream = provider
        .infer(input, CancellationToken::new())
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    let capture: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("capture.json")).unwrap()).unwrap();
    assert_eq!(capture["system"], "Use the supplied weather data.");
    assert_eq!(capture["history"][0]["message"]["role"], "user");
    assert_eq!(capture["history"][1]["message"]["role"], "assistant");
    assert_eq!(
        capture["history"][1]["message"]["content"][0],
        json!({"type":"tool_use","id":"call-prior","name":"mcp__client__lookup_weather","input":{"city":"Seoul"}})
    );
    assert!(capture["user"].is_null());
    assert_eq!(
        capture["history"][2]["message"]["content"][0],
        json!({"type":"tool_result","tool_use_id":"call-prior","content":[{"type":"text","text":"sunny"}]})
    );
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL PostgreSQL"]
async fn cancellation_and_dropped_stream_terminate_the_subprocess() {
    for mode in ["cancel", "drop", "never-polled"] {
        let (dir, provider, pool) = fixture().await;
        let cancellation = CancellationToken::new();
        let mut stream = provider
            .infer(request("cancel"), cancellation.clone())
            .await
            .unwrap();
        if mode == "never-polled" {
            tokio::time::timeout(Duration::from_secs(3), async {
                while !dir.path().join("capture.json").exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        } else {
            assert!(matches!(
                stream.next().await.unwrap().unwrap(),
                ProviderEvent::TextDelta(_)
            ));
        }
        if mode == "cancel" {
            cancellation.cancel();
            assert!(matches!(
                stream.next().await.unwrap().err().unwrap().kind,
                ErrorKind::Cancelled
            ));
        }
        drop(stream);
        provider.shutdown().await;
        assert_process_terminated(dir.path()).await;
        let saved = PgStore::new(pool)
            .claude_credentials("claude-main")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.credentials["rotation"], 1);
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL, CLAUDE_TEST_EXECUTABLE and local socket access; uses only a fake local API"]
async fn actual_cli_roundtrips_parallel_calls_against_a_local_messages_api() {
    use axum::{Json, Router, extract::State, http::header, response::IntoResponse, routing::post};
    use std::sync::{Arc, Mutex};
    let executable =
        std::env::var("CLAUDE_TEST_EXECUTABLE").expect("CLAUDE_TEST_EXECUTABLE is required");
    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let app = Router::new()
        .route("/v1/messages/count_tokens",post(|| async { Json(json!({"input_tokens":10})) }))
        .route("/v1/messages",post(|State(captured):State<Arc<Mutex<Vec<Value>>>>,Json(body):Json<Value>| async move {
            captured.lock().unwrap().push(body.clone());
            let resumed = body["messages"].as_array().unwrap().iter().any(|message| {
                message["content"].as_array().is_some_and(|blocks| blocks.iter().any(|block| block["type"]=="tool_result"))
            });
            let content = if resumed {
                vec![json!({"type":"text","text":"Forecast received."})]
            } else {
                vec![json!({"type":"tool_use","id":"toolu_seoul","name":"mcp__client__lookup_weather","input":{"city":"Seoul"}}),json!({"type":"tool_use","id":"toolu_busan","name":"mcp__client__lookup_weather","input":{"city":"Busan"}})]
            };
            let message = json!({"id":if resumed {"msg_resumed"}else{"msg_initial"},"type":"message","role":"assistant","model":body["model"],"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":0}});
            let mut events=vec![json!({"type":"message_start","message":message})];
            for (index,block) in content.iter().enumerate() {
                if block["type"]=="text" {
                    events.push(json!({"type":"content_block_start","index":index,"content_block":{"type":"text","text":""}}));
                    events.push(json!({"type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":block["text"]}}));
                } else {
                    events.push(json!({"type":"content_block_start","index":index,"content_block":{"type":"tool_use","id":block["id"],"name":block["name"],"input":{}}}));
                    events.push(json!({"type":"content_block_delta","index":index,"delta":{"type":"input_json_delta","partial_json":block["input"].to_string()}}));
                }
                events.push(json!({"type":"content_block_stop","index":index}));
            }
            events.push(json!({"type":"message_delta","delta":{"stop_reason":if resumed {"end_turn"}else{"tool_use"},"stop_sequence":null},"usage":{"output_tokens":12}}));
            events.push(json!({"type":"message_stop"}));
            let payload=events.iter().map(|event|format!("event: {}\ndata: {}\n\n",event["type"].as_str().unwrap(),event)).collect::<String>();
            ([(header::CONTENT_TYPE,"text/event-stream")],payload).into_response()
        }))
        .with_state(captured.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let wrapper = dir.path().join("claude-wrapper");
    let executable_json = serde_json::to_string(&executable).unwrap();
    std::fs::write(&wrapper,format!("#!/usr/bin/env python3\nimport os,sys\nos.environ['ANTHROPIC_API_KEY']='sk-ant-api03-local-fixture-not-a-real-key'\nos.environ['ANTHROPIC_BASE_URL']='http://{address}'\nos.execv({executable_json},[{executable_json}]+sys.argv[1:])\n")).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let pool = auth_database().await;
    let provider = ClaudeProvider::new("claude-main".into(), wrapper, PgStore::new(pool));
    let mut initial = request("claude-sonnet-4-6");
    initial.tools = vec![function()];
    let mut stream = provider
        .infer(initial, CancellationToken::new())
        .await
        .unwrap();
    let mut calls = Vec::new();
    let mut usage = None;
    let mut handoff = false;
    while let Some(event) = tokio::time::timeout(Duration::from_secs(30), stream.next())
        .await
        .expect("CLI handoff timed out")
    {
        match event.unwrap() {
            ProviderEvent::ToolCallDelta {
                id,
                name,
                arguments,
                ..
            } => calls.push(ToolCall {
                id: id.unwrap(),
                name: name.unwrap(),
                arguments,
            }),
            ProviderEvent::Usage(value) => usage = Some((value.input_tokens, value.output_tokens)),
            ProviderEvent::Finished(FinishReason::ToolCalls) => handoff = true,
            _ => {}
        }
    }
    assert!(handoff);
    assert_eq!(calls.len(), 2);
    assert_eq!(usage, Some((10, 12)));
    assert_eq!(calls[0].id, "toolu_seoul");
    assert_eq!(calls[1].id, "toolu_busan");
    let mut followup = request("claude-sonnet-4-6");
    followup.tools = vec![function()];
    followup.messages.push(Message {
        role: Role::Assistant,
        content: String::new(),
        tool_calls: calls,
        tool_call_id: None,
    });
    for (id, result) in [("toolu_seoul", "sunny"), ("toolu_busan", "cloudy")] {
        followup.messages.push(Message {
            role: Role::Tool,
            content: result.into(),
            tool_calls: vec![],
            tool_call_id: Some(id.into()),
        });
    }
    let mut stream = provider
        .infer(followup, CancellationToken::new())
        .await
        .unwrap();
    let mut text = String::new();
    let mut finished = false;
    while let Some(event) = tokio::time::timeout(Duration::from_secs(30), stream.next())
        .await
        .expect("CLI resume timed out")
    {
        match event.unwrap() {
            ProviderEvent::TextDelta(delta) => text.push_str(&delta),
            ProviderEvent::Finished(FinishReason::Stop) => finished = true,
            _ => {}
        }
    }
    assert_eq!(text, "Forecast received.");
    assert!(finished);
    let requests = captured.lock().unwrap();
    let last = requests.last().unwrap();
    let messages = last["messages"].as_array().unwrap();
    assert!(messages.iter().any(|message| message["role"] == "assistant"
        && message["content"].as_array().is_some_and(|blocks| {
            blocks
                .iter()
                .filter(|block| block["type"] == "tool_use")
                .count()
                == 2
        })));
    assert_eq!(
        messages.len(),
        3,
        "CLI inserted an unexpected conversation turn"
    );
    assert_eq!(messages[2]["role"], "user");
    let results = messages[2]["content"].as_array().unwrap();
    for (id, expected) in [("toolu_seoul", "sunny"), ("toolu_busan", "cloudy")] {
        let result = results
            .iter()
            .find(|block| block["tool_use_id"] == id)
            .expect("client result was lost");
        assert_eq!(result["type"], "tool_result");
        assert!(result.get("is_error").is_none());
        let content = result["content"].as_array().unwrap();
        let text = content[0]["text"].as_str().unwrap();
        // Native resume separates environment reminders with one trailing newline.
        assert_eq!(text.strip_suffix('\n').unwrap_or(text), expected);
        assert!(content[1..].iter().all(|block| {
            block["text"]
                .as_str()
                .unwrap()
                .starts_with("<system-reminder>")
        }));
    }
    server.abort();
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL PostgreSQL"]
async fn concurrent_calls_reload_rotated_credentials_and_persistence_errors_fail_requests() {
    let (dir, provider, pool) = fixture().await;
    let first = async {
        let mut stream = provider
            .infer(request("success"), CancellationToken::new())
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    };
    let second = async {
        let mut stream = provider
            .infer(request("success"), CancellationToken::new())
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    };
    tokio::join!(first, second);
    let store = PgStore::new(pool.clone());
    let saved = store
        .claude_credentials("claude-main")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.credentials["rotation"], 2);
    assert_eq!(
        saved.oauth_account.unwrap()["accountUuid"],
        "fixture-account"
    );
    let capture: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("capture.json")).unwrap()).unwrap();
    assert_eq!(capture["rotation"], 2);
    assert!(!Path::new(capture["config_dir"].as_str().unwrap()).exists());

    let cancellation = CancellationToken::new();
    let mut stream = provider
        .infer(request("pause"), cancellation.clone())
        .await
        .unwrap();
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        ProviderEvent::TextDelta(_)
    ));
    sqlx::query("DELETE FROM claude_credentials WHERE provider_id='claude-main'")
        .execute(&pool)
        .await
        .unwrap();
    std::fs::write(dir.path().join("release"), "ready").unwrap();
    let mut failed = false;
    while let Some(event) = stream.next().await {
        match event {
            Err(error) => {
                assert!(matches!(error.kind, ErrorKind::Persistence));
                failed = true;
            }
            Ok(ProviderEvent::Finished(_)) => panic!("DB failure must not complete successfully"),
            Ok(_) => {}
        }
    }
    assert!(failed);
    provider.shutdown().await;
    assert_process_terminated(dir.path()).await;
}
