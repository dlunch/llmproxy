use std::{os::unix::fs::PermissionsExt, path::Path, sync::Arc, time::Duration};

use futures_util::StreamExt;
use llmproxy::{
    core::{
        ErrorKind, FinishReason, FunctionDefinition, InferenceRequest, Message, Provider,
        ProviderEvent, Role, Tool, ToolCall, ToolChoice, WebSearchStatus,
    },
    providers::claude::{ClaudeAuth, ClaudeProvider},
    store::{ClaudeCredentials, PgStore},
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
assert not (config/'.credentials.json').exists()
assert os.environ['CLAUDE_CODE_OAUTH_TOKEN']
assert os.environ['CLAUDE_CODE_ENTRYPOINT']=='local-agent'
assert os.environ['CLAUDE_CODE_SDK_HAS_OAUTH_REFRESH']=='1'
assert json.loads((config/'.claude.json').read_text())['oauthAccount']['accountUuid']=='fixture-account'
if arg('--model')=='concurrent':
 (capture/f'ready-{os.getpid()}').touch()
 while len(list(capture.glob('ready-*')))<2: time.sleep(0.01)
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
(capture/'capture.json').write_text(json.dumps({'access_token':os.environ['CLAUDE_CODE_OAUTH_TOKEN'],'config_dir':str(config),'history':history,'user':user,'functions':functions,'tools':arg('--tools'),'system':pathlib.Path(arg('--system-prompt-file')).read_text()}))
scenario=arg('--model')
if scenario.startswith('refresh'):
 if scenario=='refresh-concurrent':
  (capture/f'auth-ready-{os.getpid()}').touch()
  while len(list(capture.glob('auth-ready-*')))<2: time.sleep(0.01)
 if scenario=='refresh-delayed':
  (capture/'auth-delayed').touch()
  while not (capture/'auth-release').exists(): time.sleep(0.01)
 previous=os.environ['CLAUDE_CODE_OAUTH_TOKEN']
 for attempt in range(2 if scenario=='refresh-twice' else 1):
  emit({'type':'control_request','request_id':f'refresh-{attempt}','request':{'subtype':'oauth_token_refresh'}})
  reply=read()
  assert reply['response']['response']['accessToken']!=previous
  previous=reply['response']['response']['accessToken']
  (capture/f'auth-reply-{os.getpid()}-{attempt}').write_text(reply['response']['response']['accessToken'])
reset=int(time.time())
if scenario=='quota-invalid':
 emit({'type':'rate_limit_event','rate_limit_info':{'unifiedWindows':{'five_hour':{'utilization':'unknown','resetsAt':reset+3600},'seven_day':{'utilization':0.4,'resetsAt':reset+604800},'unknown':{'utilization':1,'resetsAt':reset+3600}}}})
elif scenario!='quota-absent':
 emit({'type':'rate_limit_event','rate_limit_info':{'status':'allowed','isUsingOverage':True,'unifiedWindows':{'five_hour':{'utilization':0.25,'resetsAt':reset+3600},'seven_day':{'utilization':1.2,'resetsAt':reset+604800}}}})
 emit({'type':'rate_limit_event','rate_limit_info':{'unifiedWindows':{'five_hour':{'utilization':0.5,'resetsAt':reset+3600},'seven_day':{'utilization':-1,'resetsAt':'invalid'}}}})
emit({'type':'rate_limit_event','rate_limit_info':{'status':'rejected','rateLimitType':'five_hour','utilization':1}})
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
    let provider = ClaudeProvider::new(
        "claude-main".into(),
        script,
        PgStore::new(pool.clone()),
        ClaudeAuth::new(reqwest::Client::new(), "http://127.0.0.1:1/token".into()),
        Arc::new(llmproxy::metrics::Metrics::default()),
    );
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
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
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
        .bind(json!({"claudeAiOauth":{"accessToken":"fixture-access","refreshToken":"fixture-refresh","scopes":["user:inference"],"subscriptionType":"max","rateLimitTier":"fixture-tier"}})).bind(json!({"accountUuid":"fixture-account"})).execute(&pool).await.unwrap();
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
    for scenario in [
        "success",
        "quota-invalid",
        "quota-absent",
        "nonzero",
        "malformed",
        "truncated",
    ] {
        let (_dir, provider, pool) = fixture().await;
        let original_timestamp: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
            "SELECT updated_at FROM claude_credentials WHERE provider_id='claude-main'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
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
        let windows: Vec<(String, chrono::DateTime<chrono::Utc>, f64, chrono::DateTime<chrono::Utc>)> = sqlx::query_as("SELECT window_key,observed_at,remaining_fraction,resets_at FROM provider_quota ORDER BY window_key")
            .fetch_all(&pool).await.unwrap();
        match scenario {
            "quota-absent" => assert!(windows.is_empty()),
            "quota-invalid" => {
                assert_eq!(windows.len(), 1);
                assert_eq!(windows[0].0, "claude_seven_day");
                assert_eq!(windows[0].2, 0.6);
            }
            _ => {
                assert_eq!(windows.len(), 2);
                assert_eq!(windows[0].0, "claude_five_hour");
                assert_eq!(windows[0].2, 0.5);
                assert_eq!(windows[1].0, "claude_seven_day");
                assert_eq!(windows[1].2, 0.0);
                assert!(
                    windows[0].1 > windows[1].1,
                    "partial updates must retain each window's observation time"
                );
                assert!(windows[0].3 > chrono::Utc::now());
                assert!(windows[1].3 < chrono::Utc::now() + chrono::Duration::days(8));
            }
        }
        if matches!(scenario, "success" | "quota-invalid" | "quota-absent") {
            assert_eq!(text, "hello");
            assert!(finished);
            assert!(!failed);
            assert_eq!(usage, Some((6, 4)));
            let after: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
                "SELECT updated_at FROM claude_credentials WHERE provider_id='claude-main'",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(
                after, original_timestamp,
                "inference must not rewrite host-owned credentials"
            );
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
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM provider_quota")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    let capture: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("capture.json")).unwrap()).unwrap();
    assert_eq!(capture["functions"][0]["name"], "lookup_weather");
    assert_eq!(
        capture["functions"][0]["inputSchema"]["required"],
        json!(["city"])
    );
    assert!(!dir.path().join("unexpected-tool-response").exists());
    assert_process_terminated(dir.path()).await;
    let saved = sqlx::query_as::<_, ClaudeCredentials>("SELECT credentials,oauth_account,user_id,has_completed_onboarding,updated_at FROM claude_credentials WHERE provider_id='claude-main'")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(
        saved.credentials["claudeAiOauth"]["accessToken"],
        "fixture-access"
    );
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
        let saved = sqlx::query_as::<_, ClaudeCredentials>("SELECT credentials,oauth_account,user_id,has_completed_onboarding,updated_at FROM claude_credentials WHERE provider_id='claude-main'")
        .fetch_one(&pool).await.unwrap();
        assert_eq!(
            saved.credentials["claudeAiOauth"]["accessToken"],
            "fixture-access"
        );
        if mode != "never-polled" {
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM provider_quota")
                    .fetch_one(&pool)
                    .await
                    .unwrap(),
                2
            );
        }
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL, CLAUDE_TEST_EXECUTABLE and local socket access; uses only a fake local API"]
async fn actual_cli_roundtrips_parallel_calls_against_a_local_messages_api() {
    use axum::{Json, Router, extract::State, http::header, routing::post};
    use std::sync::{Arc, Mutex};
    let executable =
        std::env::var("CLAUDE_TEST_EXECUTABLE").expect("CLAUDE_TEST_EXECUTABLE is required");
    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let refreshes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = refreshes.clone();
    let app = Router::new()
        .route("/v1/messages/count_tokens",post(|| async { Json(json!({"input_tokens":10})) }))
        .route("/oauth/token",post(move |Json(body):Json<Value>| {
            let counter = counter.clone();
            async move {
                assert_eq!(body["refresh_token"], "fixture-refresh");
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Json(json!({"access_token":"official-renewed","refresh_token":"official-rotated","expires_in":3600}))
            }
        }))
        .route("/v1/messages",post(|State(captured):State<Arc<Mutex<Vec<Value>>>>,headers:axum::http::HeaderMap,Json(body):Json<Value>| async move {
            if headers["authorization"] == "Bearer fixture-access" {
                return axum::response::Response::builder().status(401).header("content-type","application/json")
                    .body(axum::body::Body::from(r#"{"type":"error","error":{"type":"authentication_error","message":"OAuth token has expired"}}"#)).unwrap();
            }
            assert_eq!(headers["authorization"], "Bearer official-renewed");
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
            axum::response::Response::builder()
                .header(header::CONTENT_TYPE,"text/event-stream")
                .header("anthropic-ratelimit-unified-status","allowed")
                .header("anthropic-ratelimit-unified-5h-utilization","0.25")
                .header("anthropic-ratelimit-unified-5h-reset",(chrono::Utc::now().timestamp()+3600).to_string())
                .header("anthropic-ratelimit-unified-7d-utilization","0.4")
                .header("anthropic-ratelimit-unified-7d-reset",(chrono::Utc::now().timestamp()+604800).to_string())
                .body(axum::body::Body::from(payload)).unwrap()
        }))
        .with_state(captured.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let wrapper = dir.path().join("claude-wrapper");
    let executable_json = serde_json::to_string(&executable).unwrap();
    std::fs::write(&wrapper,format!("#!/usr/bin/env python3\nimport os,sys\nos.environ['ANTHROPIC_BASE_URL']='http://{address}'\nos.execv({executable_json},[{executable_json}]+sys.argv[1:])\n")).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let pool = auth_database().await;
    let provider = ClaudeProvider::new(
        "claude-main".into(),
        wrapper,
        PgStore::new(pool.clone()),
        ClaudeAuth::new(
            reqwest::Client::new(),
            format!("http://{address}/oauth/token"),
        ),
        Arc::new(llmproxy::metrics::Metrics::default()),
    );
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
    let windows: Vec<(String, f64)> = sqlx::query_as(
        "SELECT window_key,remaining_fraction FROM provider_quota ORDER BY window_key",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        windows,
        vec![
            ("claude_five_hour".into(), 0.75),
            ("claude_seven_day".into(), 0.6)
        ]
    );
    {
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
    }
    assert_eq!(refreshes.load(std::sync::atomic::Ordering::SeqCst), 1);
    let persisted: Value = sqlx::query_scalar(
        "SELECT credentials FROM claude_credentials WHERE provider_id='claude-main'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        persisted["claudeAiOauth"]["refreshToken"],
        "official-rotated"
    );
    server.abort();
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL PostgreSQL"]
async fn concurrent_calls_share_credentials_and_persist_quota_independently() {
    let (dir, provider, pool) = fixture().await;
    let second_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let second_provider = ClaudeProvider::new(
        "claude-main".into(),
        dir.path().join("claude"),
        PgStore::new(second_pool),
        ClaudeAuth::new(reqwest::Client::new(), "http://127.0.0.1:1/token".into()),
        Arc::new(llmproxy::metrics::Metrics::default()),
    );
    let first = async {
        let mut stream = provider
            .infer(request("concurrent"), CancellationToken::new())
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    };
    let second = async {
        let mut stream = second_provider
            .infer(request("concurrent"), CancellationToken::new())
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(first, second);
    })
    .await
    .unwrap();
    let store = PgStore::new(pool.clone());
    let (held, _) = store.lock_claude_credentials("claude-main").await.unwrap();
    let mut unlocked = tokio::time::timeout(
        Duration::from_secs(1),
        second_provider.infer(request("success"), CancellationToken::new()),
    )
    .await
    .expect("fresh Claude credentials waited for a row lock")
    .unwrap();
    while let Some(event) = unlocked.next().await {
        event.unwrap();
    }
    held.commit().await.unwrap();
    let saved = sqlx::query_as::<_, ClaudeCredentials>("SELECT credentials,oauth_account,user_id,has_completed_onboarding,updated_at FROM claude_credentials WHERE provider_id='claude-main'")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(
        saved.credentials["claudeAiOauth"]["accessToken"],
        "fixture-access"
    );
    assert_eq!(
        saved.oauth_account.unwrap()["accountUuid"],
        "fixture-account"
    );
    let capture: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("capture.json")).unwrap()).unwrap();
    assert_eq!(capture["access_token"], "fixture-access");
    assert!(!Path::new(capture["config_dir"].as_str().unwrap()).exists());

    sqlx::raw_sql("CREATE FUNCTION reject_quota_update() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fixture rejection'; END $$; CREATE TRIGGER reject_quota_update BEFORE INSERT OR UPDATE ON provider_quota FOR EACH ROW EXECUTE FUNCTION reject_quota_update();")
        .execute(&pool).await.unwrap();
    let mut stream = provider
        .infer(request("success"), CancellationToken::new())
        .await
        .unwrap();
    let mut quota_failed = false;
    while let Some(event) = tokio::time::timeout(Duration::from_secs(3), stream.next())
        .await
        .unwrap()
    {
        match event {
            Err(error) => {
                assert!(matches!(error.kind, ErrorKind::Persistence));
                quota_failed = true;
            }
            Ok(ProviderEvent::Finished(_)) => {
                panic!("Quota persistence failure must not complete successfully")
            }
            Ok(_) => {}
        }
    }
    assert!(quota_failed);
    let rotated: Value = sqlx::query_scalar(
        "SELECT credentials FROM claude_credentials WHERE provider_id='claude-main'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        rotated["claudeAiOauth"]["accessToken"], "fixture-access",
        "quota failure must not change host-owned credentials"
    );
    sqlx::raw_sql(
        "DROP TRIGGER reject_quota_update ON provider_quota; DROP FUNCTION reject_quota_update();",
    )
    .execute(&pool)
    .await
    .unwrap();

    provider.shutdown().await;
    assert_process_terminated(dir.path()).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL PostgreSQL and local socket access"]
async fn idle_oauth_refresh_preserves_credentials_and_coordinates_replicas() {
    use axum::{
        Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::post,
    };
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    type OAuthState = (Arc<Mutex<Vec<Value>>>, Arc<AtomicUsize>);
    let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
    let mode = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route("/token", post(|State((calls, mode)): State<OAuthState>, Json(body): Json<Value>| async move {
        assert_eq!(body["grant_type"], "refresh_token");
        calls.lock().unwrap().push(body);
        tokio::time::sleep(Duration::from_millis(50)).await;
        match mode.load(Ordering::SeqCst) {
            0 => Json(json!({"access_token":"new-access","refresh_token":"rotated","expires_in":3600,"refresh_token_expires_in":86400,"scope":"user:inference custom:scope"})).into_response(),
            1 => Json(json!({"access_token":"newer-access","expires_in":3600})).into_response(),
            2 => (StatusCode::UNAUTHORIZED, "SECRET rejected token").into_response(),
            3 => "{SECRET malformed".into_response(),
            4 => Json(json!({"access_token":"SECRET missing-expiry"})).into_response(),
            _ => "x".repeat(1024 * 1024 + 1).into_response(),
        }
    })).with_state((calls.clone(), mode.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let pool = auth_database().await;
    let second_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let first = ClaudeProvider::new(
        "claude-main".into(),
        "/must-not-run-claude".into(),
        PgStore::new(pool.clone()),
        ClaudeAuth::new(reqwest::Client::new(), endpoint.clone()),
        Arc::new(llmproxy::metrics::Metrics::default()),
    );
    let second = ClaudeProvider::new(
        "claude-main".into(),
        "/must-not-run-claude".into(),
        PgStore::new(second_pool),
        ClaudeAuth::new(reqwest::Client::new(), endpoint),
        Arc::new(llmproxy::metrics::Metrics::default()),
    );
    let imported = json!({"otherCredential":{"retain":true},"claudeAiOauth":{"accessToken":"old-access","refreshToken":"old-refresh","expiresAt":0,"refreshTokenExpiresAt":123456,"clientId":"custom-client","scopes":["user:inference","custom:scope"],"subscriptionType":"max","unknown":{"retain":true}}});
    sqlx::query("UPDATE claude_credentials SET credentials=$1 WHERE provider_id='claude-main'")
        .bind(&imported)
        .execute(&pool)
        .await
        .unwrap();
    let (a, b) = tokio::join!(first.refresh_credentials(), second.refresh_credentials());
    a.unwrap();
    b.unwrap();
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(calls.lock().unwrap()[0]["client_id"], "custom-client");
    assert_eq!(
        calls.lock().unwrap()[0]["scope"],
        "user:inference custom:scope"
    );
    let updated: Value = sqlx::query_scalar(
        "SELECT credentials FROM claude_credentials WHERE provider_id='claude-main'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(updated["otherCredential"], imported["otherCredential"]);
    assert_eq!(
        updated["claudeAiOauth"]["unknown"],
        imported["claudeAiOauth"]["unknown"]
    );
    assert_eq!(updated["claudeAiOauth"]["subscriptionType"], "max");
    assert_eq!(updated["claudeAiOauth"]["refreshToken"], "rotated");
    assert!(
        updated["claudeAiOauth"]["expiresAt"].as_i64().unwrap()
            > chrono::Utc::now().timestamp_millis()
    );
    first.refresh_credentials().await.unwrap();
    assert_eq!(calls.lock().unwrap().len(), 1);

    mode.store(1, Ordering::SeqCst);
    sqlx::query("UPDATE claude_credentials SET credentials=jsonb_set(credentials,'{claudeAiOauth,expiresAt}','null'),updated_at=now()-interval '2 hours' WHERE provider_id='claude-main'").execute(&pool).await.unwrap();
    first.refresh_credentials().await.unwrap();
    let refreshed: Value = sqlx::query_scalar(
        "SELECT credentials FROM claude_credentials WHERE provider_id='claude-main'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(refreshed["claudeAiOauth"]["refreshToken"], "rotated");
    assert_eq!(
        refreshed["claudeAiOauth"]["refreshTokenExpiresAt"],
        updated["claudeAiOauth"]["refreshTokenExpiresAt"]
    );
    assert_eq!(
        refreshed["claudeAiOauth"]["scopes"],
        updated["claudeAiOauth"]["scopes"]
    );
    assert_eq!(calls.lock().unwrap().len(), 2);

    for failure in 2..=5 {
        mode.store(failure, Ordering::SeqCst);
        sqlx::query("UPDATE claude_credentials SET credentials=$1 WHERE provider_id='claude-main'")
            .bind(&imported)
            .execute(&pool)
            .await
            .unwrap();
        let error = first
            .refresh_credentials()
            .await
            .expect_err("invalid refresh must fail");
        assert!(matches!(error.kind, ErrorKind::UpstreamAuth));
        assert!(!error.message.contains("SECRET"));
        let retained: Value = sqlx::query_scalar(
            "SELECT credentials FROM claude_credentials WHERE provider_id='claude-main'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(retained, imported);
    }
    mode.store(1, Ordering::SeqCst);
    first.refresh_credentials().await.unwrap();
    sqlx::query("UPDATE claude_credentials SET credentials=$1 WHERE provider_id='claude-main'")
        .bind(json!({"claudeAiOauth":{"accessToken":"old","refreshToken":"default-refresh","expiresAt":0}})).execute(&pool).await.unwrap();
    first.refresh_credentials().await.unwrap();
    let default_call = calls.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        default_call["client_id"],
        "9d1c250a-e61b-44d9-88ed-5944d1962f5e"
    );
    assert_eq!(
        default_call["scope"],
        "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload user:plugins"
    );
    let successful_calls = calls.lock().unwrap().len();
    let store = PgStore::new(pool.clone());
    let (lock, _) = store.lock_claude_credentials("claude-main").await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), second.refresh_credentials())
        .await
        .unwrap()
        .unwrap();
    lock.rollback().await.unwrap();
    sqlx::query("UPDATE claude_credentials SET credentials=$1 WHERE provider_id='claude-main'")
        .bind(&imported)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE accounts SET enabled=false")
        .execute(&pool)
        .await
        .unwrap();
    first.refresh_credentials().await.unwrap();
    sqlx::query("UPDATE accounts SET enabled=true")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE claude_credentials SET credentials='{}'")
        .execute(&pool)
        .await
        .unwrap();
    first.refresh_credentials().await.unwrap();
    sqlx::query("DELETE FROM claude_credentials WHERE provider_id='claude-main'")
        .execute(&pool)
        .await
        .unwrap();
    first.refresh_credentials().await.unwrap();
    assert_eq!(calls.lock().unwrap().len(), successful_calls);
    server.abort();
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL PostgreSQL and local socket access"]
async fn oauth_callbacks_coordinate_replicas_and_persist_after_cancellation() {
    use axum::{Json, Router, routing::post};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (directory, _, pool) = fixture().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let (counter, arrived, proceed) = (calls.clone(), started.clone(), release.clone());
    let app = Router::new().route("/token", post(move |Json(body): Json<Value>| {
        let (counter, arrived, proceed) = (counter.clone(), arrived.clone(), proceed.clone());
        async move {
            assert_eq!(body["grant_type"], "refresh_token");
            assert!(body["refresh_token"].as_str().unwrap().starts_with("fixture-refresh")
                || body["refresh_token"].as_str().unwrap().starts_with("rotated-"));
            let number = counter.fetch_add(1, Ordering::SeqCst) + 1;
            arrived.add_permits(1);
            proceed.acquire().await.unwrap().forget();
            Json(json!({"access_token":format!("renewed-{number}"),"refresh_token":format!("rotated-{number}"),"expires_in":3600}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/token", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let other_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let metrics = Arc::new(llmproxy::metrics::Metrics::default());
    let first = Arc::new(ClaudeProvider::new(
        "claude-main".into(),
        directory.path().join("claude"),
        PgStore::new(pool.clone()),
        ClaudeAuth::new(reqwest::Client::new(), url.clone()),
        metrics.clone(),
    ));
    let second = Arc::new(ClaudeProvider::new(
        "claude-main".into(),
        directory.path().join("claude"),
        PgStore::new(other_pool),
        ClaudeAuth::new(reqwest::Client::new(), url),
        metrics.clone(),
    ));
    let mut jobs = Vec::new();
    for provider in [first.clone(), second.clone()] {
        jobs.push(tokio::spawn(async move {
            let mut stream = provider
                .infer(request("refresh-concurrent"), CancellationToken::new())
                .await
                .unwrap();
            let mut finished = false;
            while let Some(event) = stream.next().await {
                if matches!(event.unwrap(), ProviderEvent::Finished(_)) {
                    finished = true;
                }
            }
            assert!(finished);
        }));
    }
    tokio::time::timeout(Duration::from_secs(3), started.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    // Maintenance and inference share refresh ownership even with single-connection pools.
    let maintenance = {
        let provider = second.clone();
        tokio::spawn(async move {
            provider.refresh_credentials().await.unwrap();
        })
    };
    release.add_permits(1);
    for job in jobs {
        tokio::time::timeout(Duration::from_secs(5), job)
            .await
            .unwrap()
            .unwrap();
    }
    maintenance.await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // A child with an older token reuses an already persisted replacement.
    let mut delayed = first
        .infer(request("refresh-delayed"), CancellationToken::new())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !directory.path().join("auth-delayed").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    sqlx::query("UPDATE claude_credentials SET credentials=jsonb_set(credentials,'{claudeAiOauth,accessToken}',$1) WHERE provider_id='claude-main'")
        .bind(json!("newer-from-another-replica")).execute(&pool).await.unwrap();
    std::fs::write(directory.path().join("auth-release"), "ready").unwrap();
    while let Some(event) = delayed.next().await {
        event.unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // A second rejection refers to the token last returned to that child.
    release.add_permits(2);
    let mut twice = first
        .infer(request("refresh-twice"), CancellationToken::new())
        .await
        .unwrap();
    while let Some(event) = twice.next().await {
        event.unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    started.acquire_many(2).await.unwrap().forget();

    sqlx::raw_sql("CREATE FUNCTION reject_auth_update() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fixture rejection'; END $$; CREATE TRIGGER reject_auth_update BEFORE UPDATE ON claude_credentials FOR EACH ROW EXECUTE FUNCTION reject_auth_update();")
        .execute(&pool).await.unwrap();
    release.add_permits(1);
    let mut failure = first
        .infer(request("refresh-once"), CancellationToken::new())
        .await
        .unwrap();
    let error = failure.next().await.unwrap().err().unwrap();
    assert!(matches!(error.kind, ErrorKind::Persistence));
    assert!(failure.next().await.is_none());
    started.acquire().await.unwrap().forget();
    sqlx::raw_sql("DROP TRIGGER reject_auth_update ON claude_credentials; DROP FUNCTION reject_auth_update();")
        .execute(&pool).await.unwrap();

    // Due request preparation and maintenance use the same refresh owner.
    sqlx::query("UPDATE claude_credentials SET credentials=jsonb_set(credentials,'{claudeAiOauth,expiresAt}','0') WHERE provider_id='claude-main'")
        .execute(&pool).await.unwrap();
    release.add_permits(1);
    let (a, b, maintained) = tokio::join!(
        first.infer(request("success"), CancellationToken::new()),
        second.infer(request("success"), CancellationToken::new()),
        first.refresh_credentials(),
    );
    maintained.unwrap();
    for mut stream in [a.unwrap(), b.unwrap()] {
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 5);
    started.acquire().await.unwrap().forget();

    let cancellation = CancellationToken::new();
    let stream = first
        .infer(request("refresh-once"), cancellation.clone())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), started.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    cancellation.cancel();
    drop(stream);
    release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(3), first.shutdown())
        .await
        .unwrap();
    let persisted: Value = sqlx::query_scalar(
        "SELECT credentials FROM claude_credentials WHERE provider_id='claude-main'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(persisted["claudeAiOauth"]["refreshToken"], "rotated-6");
    assert_process_terminated(directory.path()).await;
    second.shutdown().await;
    let rendered = metrics.render().unwrap();
    let observed: u64 = ["credential_refresh", "maintenance_credential_refresh"]
        .iter()
        .map(|stage| {
            let prefix = format!("llmproxy_stage_duration_seconds_count{{stage=\"{stage}\"}} ");
            rendered
                .lines()
                .find_map(|line| line.strip_prefix(&prefix))
                .map(|value| value.parse::<u64>().unwrap())
                .unwrap_or(0)
        })
        .sum();
    assert_eq!(
        observed, 6,
        "refresh metrics must include persistence failure and cancelled callers"
    );
    server.abort();
}
