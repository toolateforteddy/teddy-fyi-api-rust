//! Tests for the Gemini spend budget and for what actually goes on the wire.
//!
//! Split in two by dependency. The budget tests need Redis and **skip** rather
//! than fail when it is not reachable, so a local `cargo test` with no cache-svc
//! running is still useful; CI runs them for real. The wire tests need nothing
//! but a loopback socket: they stand up a one-request stub server and inspect the
//! bytes, which is the only way to assert a *negative* — that the API key is not
//! in the URL.

use std::time::Duration;

use crate::routes::ai::budget::{
    charge_gemini_call_on_day, kill_switch_is_set, BudgetLimits, BudgetRefusal, KILL_SWITCH_KEY,
    DEFAULT_MAX_CALLS_PER_DAY, DEFAULT_MAX_CALLS_PER_USER_PER_DAY,
};
use crate::routes::ai::gemini::{build_http_client_with_timeout, call_gemini_at, redact};
use crate::routes::ai::handlers::{check_title_length, MAX_TITLE_CHARS};
use crate::routes::ai::types::CategorizeItemResponse;
use crate::routes::sync::types::AppError;

// ---------------------------------------------------------------------------
// Title length: characters, not bytes
// ---------------------------------------------------------------------------

#[test]
fn empty_or_whitespace_title_is_rejected() {
    let err_empty = check_title_length("item_title", "").unwrap_err();
    match err_empty {
        AppError::BadRequest(msg) => assert!(msg.contains("item_title must not be empty")),
        other => panic!("expected BadRequest for empty title, got {:?}", other),
    }

    let err_whitespace = check_title_length("todo_title", "   ").unwrap_err();
    match err_whitespace {
        AppError::BadRequest(msg) => assert!(msg.contains("todo_title must not be empty")),
        other => panic!("expected BadRequest for whitespace title, got {:?}", other),
    }
}

#[test]
fn title_length_is_measured_in_characters_not_bytes() {
    // Exactly at the bound in characters, but 300 bytes in UTF-8. The old
    // `len() > 100` byte check rejected this while accepting 100 ASCII
    // characters — the same title in Japanese was allowed a third of the length.
    let multibyte: String = "あ".repeat(MAX_TITLE_CHARS);
    assert_eq!(multibyte.len(), MAX_TITLE_CHARS * 3);
    assert!(check_title_length("item_title", &multibyte).is_ok());

    // ASCII at the bound is still fine, and one past it is not — in either
    // alphabet, which is the whole point.
    assert!(check_title_length("item_title", &"a".repeat(MAX_TITLE_CHARS)).is_ok());
    assert!(check_title_length("item_title", &"a".repeat(MAX_TITLE_CHARS + 1)).is_err());
    assert!(check_title_length("item_title", &"あ".repeat(MAX_TITLE_CHARS + 1)).is_err());
}

#[test]
fn an_over_long_title_is_a_bad_request_naming_the_unit() {
    let err = check_title_length("todo_title", &"a".repeat(MAX_TITLE_CHARS + 1)).unwrap_err();
    match err {
        AppError::BadRequest(message) => {
            assert!(message.contains("todo_title"), "message: {}", message);
            assert!(message.contains("characters"), "message: {}", message);
        }
        other => panic!("expected BadRequest, got {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// Limits and the kill switch value parsing
// ---------------------------------------------------------------------------

#[test]
fn default_limits_are_the_documented_numbers() {
    let limits = BudgetLimits::default();
    assert_eq!(limits.per_user_per_day, DEFAULT_MAX_CALLS_PER_USER_PER_DAY);
    assert_eq!(limits.total_per_day, DEFAULT_MAX_CALLS_PER_DAY);
}

#[test]
fn kill_switch_reads_any_truthy_spelling_as_disabled() {
    // Absent, or explicitly off: Gemini stays on.
    assert!(!kill_switch_is_set(None));
    for off in ["0", "false", "off", "no", "", "  OFF  "] {
        assert!(!kill_switch_is_set(Some(off)), "expected {:?} to be off", off);
    }
    // Anything else stops spend. An operator typing `SET ... yes` under pressure
    // must not silently get a no-op.
    for on in ["1", "true", "yes", "on", "stop", "incident-4821"] {
        assert!(kill_switch_is_set(Some(on)), "expected {:?} to be on", on);
    }
}

// ---------------------------------------------------------------------------
// Budget enforcement (needs Redis)
// ---------------------------------------------------------------------------

/// A Redis client, or `None` with a printed note. Tests that need Redis skip
/// rather than fail locally; CI has cache-svc and exercises them for real.
async fn redis_or_skip(test: &str) -> Option<redis::Client> {
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    let client = redis::Client::open(url).ok()?;
    match client.get_multiplexed_async_connection().await {
        Ok(mut conn) => match redis::cmd("PING").query_async::<String>(&mut conn).await {
            Ok(_) => Some(client),
            Err(_) => {
                eprintln!("SKIP {}: Redis did not answer PING", test);
                None
            }
        },
        Err(_) => {
            eprintln!("SKIP {}: no Redis at REDIS_URL", test);
            None
        }
    }
}

/// Unique per run, so concurrent tests and repeated runs never share a counter.
fn unique_user() -> String {
    format!("budget-test-{}", uuid::Uuid::new_v4())
}

/// Days far enough in the past that no production key can collide with them.
const DAY_ONE: &str = "1999-01-01";
const DAY_TWO: &str = "1999-01-02";

/// Guards the kill switch, which is one process-wide Redis key that every budget
/// test reads through `charge_gemini_call_on_day`.
///
/// Both halves matter, and an earlier version had only one: the test that *sets*
/// the switch takes it exclusively, and every other budget test holds it shared
/// for as long as it is charging calls. A lock only one participant takes
/// serialises nothing — while the switch was set, any test running concurrently
/// got `Err(BudgetRefusal::KillSwitch)` where it asserted `Ok(())`, which is an
/// intermittent CI failure that passes on a re-run.
static KILL_SWITCH_LOCK: tokio::sync::RwLock<()> = tokio::sync::RwLock::const_new(());

#[tokio::test]
async fn a_user_over_budget_is_refused() {
    let Some(client) = redis_or_skip("a_user_over_budget_is_refused").await else {
        return;
    };
    // Shared: the kill switch must stay unset for the whole of this test.
    let _kill_switch = KILL_SWITCH_LOCK.read().await;
    let limits = BudgetLimits {
        per_user_per_day: 3,
        total_per_day: u64::MAX,
    };
    let user = unique_user();

    for call in 1..=3 {
        assert_eq!(
            charge_gemini_call_on_day(&client, limits, &user, DAY_ONE).await,
            Ok(()),
            "call {} should be within budget",
            call
        );
    }
    // The fourth is the first one over, and it is refused with the per-user
    // reason — which is what makes the response a 429 rather than a 503.
    assert_eq!(
        charge_gemini_call_on_day(&client, limits, &user, DAY_ONE).await,
        Err(BudgetRefusal::UserExhausted)
    );
    assert!(matches!(
        AppError::from(BudgetRefusal::UserExhausted),
        AppError::TooManyRequests(_)
    ));
}

#[tokio::test]
async fn one_user_over_budget_does_not_refuse_another() {
    let Some(client) = redis_or_skip("one_user_over_budget_does_not_refuse_another").await else {
        return;
    };
    // Shared: the kill switch must stay unset for the whole of this test.
    let _kill_switch = KILL_SWITCH_LOCK.read().await;
    let limits = BudgetLimits {
        per_user_per_day: 1,
        total_per_day: u64::MAX,
    };
    let noisy = unique_user();
    let quiet = unique_user();

    assert_eq!(
        charge_gemini_call_on_day(&client, limits, &noisy, DAY_ONE).await,
        Ok(())
    );
    assert_eq!(
        charge_gemini_call_on_day(&client, limits, &noisy, DAY_ONE).await,
        Err(BudgetRefusal::UserExhausted)
    );
    // The budget is per account, so the abuser must not take the household next
    // door down with them.
    assert_eq!(
        charge_gemini_call_on_day(&client, limits, &quiet, DAY_ONE).await,
        Ok(())
    );
}

#[tokio::test]
async fn the_budget_resets_on_the_next_day() {
    let Some(client) = redis_or_skip("the_budget_resets_on_the_next_day").await else {
        return;
    };
    // Shared: the kill switch must stay unset for the whole of this test.
    let _kill_switch = KILL_SWITCH_LOCK.read().await;
    let limits = BudgetLimits {
        per_user_per_day: 1,
        total_per_day: u64::MAX,
    };
    let user = unique_user();

    assert_eq!(
        charge_gemini_call_on_day(&client, limits, &user, DAY_ONE).await,
        Ok(())
    );
    assert_eq!(
        charge_gemini_call_on_day(&client, limits, &user, DAY_ONE).await,
        Err(BudgetRefusal::UserExhausted)
    );
    // Same account, next UTC day: a fresh key, so a fresh allowance.
    assert_eq!(
        charge_gemini_call_on_day(&client, limits, &user, DAY_TWO).await,
        Ok(())
    );
}

#[tokio::test]
async fn counters_carry_a_ttl_so_they_do_not_accumulate() {
    let Some(client) = redis_or_skip("counters_carry_a_ttl_so_they_do_not_accumulate").await else {
        return;
    };
    // Shared: the kill switch must stay unset for the whole of this test.
    let _kill_switch = KILL_SWITCH_LOCK.read().await;
    let user = unique_user();
    assert_eq!(
        charge_gemini_call_on_day(&client, BudgetLimits::default(), &user, DAY_ONE).await,
        Ok(())
    );

    let mut conn = client.get_multiplexed_async_connection().await.unwrap();
    let ttl: i64 = redis::cmd("TTL")
        .arg(format!("ai:gemini:calls:user:{}:{}", user, DAY_ONE))
        .query_async(&mut conn)
        .await
        .unwrap();
    // -1 means "no expiry", which is how a per-day counter turns into an
    // unbounded key space.
    assert!(ttl > 0, "counter should expire, TTL was {}", ttl);
}

#[tokio::test]
async fn a_global_budget_refuses_even_a_quiet_user() {
    let Some(client) = redis_or_skip("a_global_budget_refuses_even_a_quiet_user").await else {
        return;
    };
    // Shared: the kill switch must stay unset for the whole of this test.
    let _kill_switch = KILL_SWITCH_LOCK.read().await;

    // A day of its own, so the shared global counter is not perturbed by the
    // other tests in this file.
    let day = format!("1999-02-{}", &uuid::Uuid::new_v4().simple().to_string()[..6]);
    let limits = BudgetLimits {
        per_user_per_day: u64::MAX,
        total_per_day: 2,
    };

    assert_eq!(
        charge_gemini_call_on_day(&client, limits, &unique_user(), &day).await,
        Ok(())
    );
    assert_eq!(
        charge_gemini_call_on_day(&client, limits, &unique_user(), &day).await,
        Ok(())
    );
    // A third account, well inside its own budget, still gets nothing: the
    // deployment's daily ceiling is what bounds the bill.
    assert_eq!(
        charge_gemini_call_on_day(&client, limits, &unique_user(), &day).await,
        Err(BudgetRefusal::GlobalExhausted)
    );
}

#[tokio::test]
async fn the_kill_switch_stops_calls_and_releasing_it_restores_them() {
    let Some(client) =
        redis_or_skip("the_kill_switch_stops_calls_and_releasing_it_restores_them").await
    else {
        return;
    };
    let user = unique_user();
    let mut conn = client.get_multiplexed_async_connection().await.unwrap();

    // Exclusive: the switch is global by design, so for as long as it is set no
    // other budget test in this process may be charging a call.
    let _guard = KILL_SWITCH_LOCK.write().await;

    redis::cmd("SET")
        .arg(KILL_SWITCH_KEY)
        .arg("1")
        .query_async::<()>(&mut conn)
        .await
        .unwrap();

    let refused =
        charge_gemini_call_on_day(&client, BudgetLimits::default(), &user, DAY_ONE).await;
    // Cleared before asserting, so a failure cannot leave the switch set for
    // every other test in the run.
    redis::cmd("DEL")
        .arg(KILL_SWITCH_KEY)
        .query_async::<()>(&mut conn)
        .await
        .unwrap();

    assert_eq!(refused, Err(BudgetRefusal::KillSwitch));
    // 503, not 429: this is an operator action, not the caller's doing.
    assert!(matches!(
        AppError::from(BudgetRefusal::KillSwitch),
        AppError::Overloaded(_)
    ));

    assert_eq!(
        charge_gemini_call_on_day(&client, BudgetLimits::default(), &user, DAY_ONE).await,
        Ok(())
    );
}

#[tokio::test]
async fn an_unreachable_redis_refuses_rather_than_spending() {
    // Port 1 is never listening; this is the "Redis is down" path, and the point
    // is that it fails closed. A limit that disappears when the dependency does
    // is not a limit.
    let client = redis::Client::open("redis://127.0.0.1:1").unwrap();
    assert_eq!(
        charge_gemini_call_on_day(&client, BudgetLimits::default(), "someone", DAY_ONE).await,
        Err(BudgetRefusal::Unmetered)
    );
    assert!(matches!(
        AppError::from(BudgetRefusal::Unmetered),
        AppError::Overloaded(_)
    ));
}

// ---------------------------------------------------------------------------
// What goes on the wire
// ---------------------------------------------------------------------------

/// What a stub server saw: the request line plus headers, lowercased names kept
/// as sent so a header assertion is meaningful.
struct CapturedRequest {
    request_line: String,
    headers: Vec<(String, String)>,
}

impl CapturedRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Accepts exactly one request, answers with `body` as a Gemini-shaped success,
/// and hands back what it read. Hand-rolled rather than pulling in a mock-server
/// dependency for two tests.
async fn stub_gemini(body: &'static str) -> (String, tokio::task::JoinHandle<CapturedRequest>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());

    let handle = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut socket, _) = listener.accept().await.unwrap();

        // Read until the headers are complete, then the declared body. Enough of
        // HTTP/1.1 for a single JSON POST from reqwest and no more.
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let read = socket.read(&mut chunk).await.unwrap();
            if read == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..read]);
            let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
            let content_length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            if buf.len() >= head_end + 4 + content_length {
                break;
            }
        }

        let head_end = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        let mut lines = head.lines();
        let request_line = lines.next().unwrap_or_default().to_string();
        let headers = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
            .collect();

        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.flush().await.unwrap();

        CapturedRequest {
            request_line,
            headers,
        }
    });

    (base, handle)
}

/// A minimal well-formed Gemini answer carrying a `CategorizeItemResponse`.
const STUB_BODY: &str = r#"{"candidates":[{"content":{"parts":[{"text":"{\"selected_category\":\"Produce\"}"}]}}]}"#;

#[tokio::test]
async fn the_api_key_is_sent_as_a_header_and_never_in_the_url() {
    let (base, server) = stub_gemini(STUB_BODY).await;
    let client = build_http_client_with_timeout(Duration::from_secs(5));

    let response: CategorizeItemResponse = call_gemini_at(
        &base,
        &client,
        "SUPER-SECRET-KEY",
        Some("system"),
        "item_title: <<<apples>>>",
        "gemini-2.5-flash-lite",
    )
    .await
    .expect("stub should answer");
    assert_eq!(response.selected_category, "Produce");

    let captured = server.await.unwrap();
    // The URL is what proxies log and what `reqwest::Error` prints. Nothing
    // secret may appear in it — not the key, and not a `key=` parameter at all.
    assert!(
        !captured.request_line.contains("SUPER-SECRET-KEY"),
        "key leaked into the request line: {}",
        captured.request_line
    );
    assert!(
        !captured.request_line.contains("key="),
        "request line still carries a key parameter: {}",
        captured.request_line
    );
    assert!(
        captured.request_line.contains("/models/gemini-2.5-flash-lite:generateContent"),
        "unexpected request line: {}",
        captured.request_line
    );
    assert_eq!(captured.header("x-goog-api-key"), Some("SUPER-SECRET-KEY"));
}

#[tokio::test]
async fn a_hanging_gemini_surfaces_as_a_timeout_error() {
    // Accepts the connection and then says nothing at all — the hung-dependency
    // case that used to hold a handler task and its socket indefinitely.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let _accepting = tokio::spawn(async move {
        let held = listener.accept().await;
        // Held open deliberately: dropping it would answer with a connection
        // reset and test a different failure.
        std::future::pending::<()>().await;
        drop(held);
    });

    let client = build_http_client_with_timeout(Duration::from_millis(300));
    let started = std::time::Instant::now();
    let result: Result<CategorizeItemResponse, AppError> = call_gemini_at(
        &base,
        &client,
        "SUPER-SECRET-KEY",
        None,
        "item_title: <<<apples>>>",
        "gemini-2.5-flash-lite",
    )
    .await;

    match result {
        Err(AppError::Gemini(message)) => {
            assert!(message.contains("timed out"), "message: {}", message);
            assert!(
                !message.contains("SUPER-SECRET-KEY"),
                "error text carries the key: {}",
                message
            );
        }
        other => panic!("expected a Gemini timeout error, got {:?}", other.err()),
    }
    // The client deadline, not the server's 30s request deadline, is what ended
    // this — which is the whole reason the Gemini timeout is the shorter one.
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn redact_strips_the_url_a_reqwest_error_would_otherwise_print() {
    // Find a port with nothing on it, so the request fails at connect and the
    // error carries the URL it was trying.
    let closed = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        addr
    };
    let client = build_http_client_with_timeout(Duration::from_millis(500));
    let err = client
        .get(format!("http://{}/v1beta/models/x?key=SUPER-SECRET-KEY", closed))
        .send()
        .await
        .expect_err("nothing is listening");

    // This is the leak that motivated moving the key into a header, verified
    // against the reqwest in use rather than assumed: `Display` appends
    // " for url (...)" with the query string intact.
    let leaked = err.to_string();
    assert!(
        leaked.contains("SUPER-SECRET-KEY"),
        "reqwest no longer prints the URL; the redaction may be unnecessary: {}",
        leaked
    );

    let redacted = redact(err);
    assert!(
        !redacted.contains("SUPER-SECRET-KEY"),
        "redaction failed: {}",
        redacted
    );
    // Still says what went wrong; only the URL is gone.
    assert!(!redacted.is_empty());
}

// ---------------------------------------------------------------------------
// A deployment with no GEMINI_API_KEY
// ---------------------------------------------------------------------------
//
// The point of these is the split plan's risk-register entry: `init_app_state` used to
// `expect` this variable, so a ScribbleRoute deployment that dropped it crash-looped, and
// removing the feature meant editing code, `AppState` and the manifest in one go. The key
// is optional now, and these pin down what "absent" does at each of the three places that
// spend the AI budget.

/// The two endpoints refuse, with a 503 that does not pretend to be temporary.
#[sqlx::test]
async fn the_ai_endpoints_refuse_when_no_key_is_configured(pool: sqlx::PgPool) {
    use crate::routes::ai::handlers::{assign_todo_icon_handler, categorize_item_handler};
    use crate::routes::ai::types::{AssignTodoIconRequest, CategorizeItemRequest};
    use crate::routes::sync::tests::helpers::setup_state;
    use axum::extract::State;
    use axum::{Extension, Json};

    let mut state = setup_state(pool);
    state.gemini_api_key = None;

    let claims = crate::auth::tokens::Claims {
        sub: "user-1".to_string(),
        client_uuid: "client-1".to_string(),
        exp: 10_000_000_000,
        product: None,
    };

    let categorize = categorize_item_handler(
        State(state.clone()),
        Extension(claims.clone()),
        Json(CategorizeItemRequest {
            item_title: "milk".to_string(),
        }),
    )
    .await;
    match categorize {
        Err(AppError::NotConfigured(message)) => {
            assert!(message.contains("not configured"), "{message}");
        }
        other => panic!("expected NotConfigured from /categorize, got {other:?}"),
    }

    let icon = assign_todo_icon_handler(
        State(state),
        Extension(claims),
        Json(AssignTodoIconRequest {
            todo_title: "take out the bins".to_string(),
        }),
    )
    .await;
    assert!(
        matches!(icon, Err(AppError::NotConfigured(_))),
        "expected NotConfigured from /assign-icon, got {icon:?}"
    );
}

/// A malformed request still gets its 400. The refusal above is a fact about the
/// deployment, but it must not mask the one thing the client can actually fix.
#[sqlx::test]
async fn an_unconfigured_deployment_still_reports_a_bad_title(pool: sqlx::PgPool) {
    use crate::routes::ai::handlers::categorize_item_handler;
    use crate::routes::ai::types::CategorizeItemRequest;
    use crate::routes::sync::tests::helpers::setup_state;
    use axum::extract::State;
    use axum::{Extension, Json};

    let mut state = setup_state(pool);
    state.gemini_api_key = None;

    let result = categorize_item_handler(
        State(state),
        Extension(crate::auth::tokens::Claims {
            sub: "user-1".to_string(),
            client_uuid: "client-1".to_string(),
            exp: 10_000_000_000,
            product: None,
        }),
        Json(CategorizeItemRequest {
            item_title: "   ".to_string(),
        }),
    )
    .await;

    assert!(
        matches!(result, Err(AppError::BadRequest(_))),
        "an empty title is a 400 whether or not the key is configured, got {result:?}"
    );
}

/// The third spender is the one that must *not* refuse: `resolve_todo_icons` runs inside
/// `POST /api/sync`, so an absent key has to mean "no icon", not a failed sync. Any other
/// behaviour here turns a piece of missing configuration into data loss.
#[sqlx::test]
async fn syncing_a_todo_without_a_key_stores_it_without_an_icon(pool: sqlx::PgPool) {
    use crate::routes::sync::tests::helpers::{request, seed_device, setup_state};
    use crate::routes::sync::{
        parse_or_hash_uuid, sync_handler, AppJson, OperationType, SyncRequest, SyncScope,
        TodoChangeDelta,
    };
    use axum::extract::State;
    use axum::Extension;

    let mut state = setup_state(pool.clone());
    state.gemini_api_key = None;
    seed_device(&pool, parse_or_hash_uuid("user-1"), "Phone").await;

    // Built from the struct rather than hand-written JSON so the payload cannot drift out
    // of the shape the handler parses. `icon: None` is the whole point: this is exactly the
    // todo `resolve_todo_icons` would have asked Gemini about.
    let todo_data = crate::routes::sync::types::TodoItemData {
        id: "todo-no-key".to_string(),
        title: "Buy milk".to_string(),
        is_completed: false,
        created_at: 0,
        position: 0,
        scheduled_date: None,
        recurrence_rule: None,
        scheduled_at: 0,
        user_id: Some("user-1".to_string()),
        parent_id: None,
        is_daily: false,
        due_date: None,
        description: None,
        list_id: None,
        priority: 0,
        icon: None,
        sync_state: "SYNCED".to_string(),
        version: 1,
        is_deleted: false,
    };

    let body = SyncRequest {
        scope: Some(SyncScope::Todo),
        todo_changes: vec![TodoChangeDelta {
            id: "todo-no-key".to_string(),
            operation_type: OperationType::Insert,
            version: 1,
            data: Some(serde_json::to_value(&todo_data).unwrap()),
        }],
        ..request("client-1")
    };

    let claims = crate::auth::tokens::Claims {
        sub: "user-1".to_string(),
        client_uuid: "client-1".to_string(),
        exp: 10_000_000_000,
        product: None,
    };

    let _response = sync_handler(State(state), Extension(claims), AppJson(body))
        .await
        .expect("a sync must succeed on a deployment with no Gemini key");

    let icon = sqlx::query_scalar!(
        "SELECT icon FROM todo_items WHERE id = $1",
        "todo-no-key"
    )
    .fetch_one(&pool)
    .await
    .expect("the todo must have been written");

    assert!(
        icon.is_none() || icon.as_deref() == Some(""),
        "an unconfigured deployment must store the todo with no server-assigned icon, got {icon:?}"
    );
}
