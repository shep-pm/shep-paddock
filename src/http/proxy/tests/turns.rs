//! Requests for a model whose backend serves a limited number of conversations at once.

use super::*;
use crate::{book::LeaseId, engine::LeaseEvents, http::lease::render_id};

/// A backend that keeps every request open until the test drops the sender, and keeps the headers
/// each one came with
async fn fake_held() -> (String, watch::Sender<()>, Arc<Mutex<Vec<hyper::HeaderMap>>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let base = format!("http://{}", listener.local_addr().expect("local addr"));
    let (release, held) = watch::channel(());
    let seen = Arc::new(Mutex::new(Vec::new()));
    tokio::spawn({
        let seen = Arc::clone(&seen);
        async move {
            while let Ok((stream, _)) = listener.accept().await {
                let held = held.clone();
                let seen = Arc::clone(&seen);
                let service = service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                    seen.lock()
                        .expect("seen lock")
                        .push(request.headers().clone());
                    let chunks = stream::unfold(held.clone(), |mut held| async move {
                        // The sender is dropped to end the stream, so `changed` never sees a send.
                        let _ = held.changed().await;
                        None::<(Result<Frame<Bytes>, Infallible>, watch::Receiver<()>)>
                    });
                    let response = Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(StreamBody::new(chunks));
                    async move { response }
                });
                tokio::spawn(
                    hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service),
                );
            }
        }
    });
    (base, release, seen)
}

/// Takes a lease on `model` for `client`, noted `note`, and waits for the grant
async fn hold(
    paddock: &Paddock,
    client: &str,
    model: &str,
    note: &str,
    reclaimable: bool,
) -> (LeaseId, LeaseEvents) {
    let ask = LeaseRequest {
        leased: Leased::Model(model.into()),
        priority: Priority::Interactive,
        expected: None,
        max_wait: None,
        hold: Hold::Connection,
        note: Some(note.to_owned()),
        reclaimable,
        release_if_idle: None,
    };
    let mut events = paddock.engine.take_lease(client.into(), ask).await;
    loop {
        match bounded("the grant", events.recv()).await {
            Some(LeaseEvent::Granted { lease, .. }) => return (lease, events),
            Some(LeaseEvent::Waiting { .. }) => {}
            other => panic!("the lease was not granted: {other:?}"),
        }
    }
}

/// Sends a chat request for `model` as `mac-sessions`, naming `lease` if given, and returns the
/// status once the body ends.
async fn chat_naming(
    client: reqwest::Client,
    addr: SocketAddr,
    model: &str,
    lease: Option<LeaseId>,
) -> u16 {
    let mut request = client
        .post(format!("http://{addr}/v1/chat/completions"))
        .bearer_auth("k-mac")
        .body(format!(r#"{{"model":"{model}","stream":true}}"#));
    if let Some(lease) = lease {
        request = request.header("X-Paddock-Lease", render_id(lease));
    }
    let response = bounded("the response head", request.send())
        .await
        .expect("the endpoint answers");
    let status = response.status().as_u16();
    bounded("the body", response.bytes())
        .await
        .expect("the body");
    status
}

/// Waits until `count` requests wait in the engine's queue
async fn until_waiting(paddock: &Paddock, count: usize) {
    bounded("the queue", async {
        while paddock.engine.snapshot().await.waiters.len() != count {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
}

/// What the status says each waiting request waits for
async fn reasons(paddock: &Paddock) -> Vec<String> {
    let (status, body) = json_of(get(paddock, "/paddock/status").await).await;
    assert_eq!(status, 200, "{body}");
    body["waiters"]
        .as_array()
        .expect("a waiters list")
        .iter()
        .map(|waiter| waiter["reason"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// `name` on `url` with room for another model beside it
fn small(name: &str, url: &str, extra: &str) -> String {
    format!(
        r#"
[models.{name}]
backend = {{ sheep = "{name}" }}
url = "{url}"
vram = "4G"
idle = "2h"
apis = ["openai"]
{extra}
"#
    )
}

/// mac-sessions and bench-01, with `models`
fn two_clients(models: &str) -> Arc<Config> {
    paddock_config(&format!(
        "[[clients]]\nname = \"bench-01\"\nkey = \"k-bench\"\n{models}"
    ))
}

/// Sends a chat request for `iq2_xs` as `mac-sessions`, and returns the status once the body ends.
async fn chat(client: reqwest::Client, addr: SocketAddr) -> u16 {
    let response = bounded(
        "the response head",
        client
            .post(format!("http://{addr}/v1/chat/completions"))
            .bearer_auth("k-mac")
            .body(r#"{"model":"iq2_xs","stream":true}"#)
            .send(),
    )
    .await
    .expect("the endpoint answers");
    let status = response.status().as_u16();
    bounded("the body", response.bytes())
        .await
        .expect("the body");
    status
}

/// iq2_xs has one turn, so the lease mac-sessions holds gives its two requests one conversation:
/// the second waits, and the status says it waits for a turn, until the first ends.
#[tokio::test]
async fn two_concurrent_requests_under_one_lease_show_one_in_flight_and_one_waiter() {
    let (sse, chunks) = fake_sse().await;
    let models = sheep("iq2_xs", &sse, "apis = [\"openai\"]\nsequences = 1");
    with_paddock(
        paddock_config(&models),
        FakeShepherd::new(),
        |paddock| async move {
            let ask = LeaseRequest {
                leased: Leased::Model("iq2_xs".into()),
                priority: Priority::Interactive,
                expected: None,
                max_wait: None,
                hold: Hold::Connection,
                note: None,
                reclaimable: false,
                release_if_idle: None,
            };
            let mut events = paddock.engine.take_lease("mac-sessions".into(), ask).await;
            loop {
                match bounded("the grant", events.recv()).await {
                    Some(LeaseEvent::Granted { .. }) => break,
                    Some(LeaseEvent::Waiting { .. }) => {}
                    other => panic!("the lease was not granted: {other:?}"),
                }
            }

            let first = tokio::task::spawn_local(chat(paddock.client.clone(), paddock.addr));
            paddock.until_in_flight("iq2_xs", 1).await;
            let second = tokio::task::spawn_local(chat(paddock.client.clone(), paddock.addr));
            bounded("the second request queueing", async {
                while paddock.engine.snapshot().await.waiters.is_empty() {
                    sleep(Duration::from_millis(10)).await;
                }
            })
            .await;

            let (status, body) = json_of(get(&paddock, "/paddock/status").await).await;
            assert_eq!(status, 200, "{body}");
            let waiters = body["waiters"].as_array().expect("a waiters list");
            assert_eq!(waiters.len(), 1, "{body}");
            assert_eq!(waiters[0]["kind"], "request", "{body}");
            assert_eq!(waiters[0]["reason_kind"], "turn", "{body}");
            assert_eq!(paddock.in_flight("iq2_xs").await, Some(1));

            // The first request's stream ends, which frees the turn for the second.
            drop(chunks);
            assert_eq!(bounded("the first", first).await.expect("joined"), 200);
            assert_eq!(bounded("the second", second).await.expect("joined"), 200);
            drop(events);
        },
    )
    .await;
}

/// Sends a request for `model` naming `lease`, in the background
fn send_naming(
    paddock: &Paddock,
    model: &'static str,
    lease: Option<LeaseId>,
) -> tokio::task::JoinHandle<u16> {
    tokio::task::spawn_local(chat_naming(
        paddock.client.clone(),
        paddock.addr,
        model,
        lease,
    ))
}

/// Ends every request the backend holds open, and checks each was served
async fn finish(release: watch::Sender<()>, sent: Vec<tokio::task::JoinHandle<u16>>) {
    drop(release);
    for request in sent {
        assert_eq!(bounded("a request", request).await.expect("joined"), 200);
    }
}

#[tokio::test]
async fn the_lease_header_picks_that_leases_turn_when_the_client_holds_two() {
    let (base, release, _seen) = fake_held().await;
    let models = small("iq2_xs", &base, "sequences = 2");
    with_paddock(
        paddock_config(&models),
        FakeShepherd::new(),
        |paddock| async move {
            let (alpha, _alpha_events) =
                hold(&paddock, "mac-sessions", "iq2_xs", "alpha", false).await;
            let (beta, _beta_events) =
                hold(&paddock, "mac-sessions", "iq2_xs", "beta", false).await;

            let mut sent = vec![send_naming(&paddock, "iq2_xs", Some(beta))];
            paddock.until_in_flight("iq2_xs", 1).await;
            sent.push(send_naming(&paddock, "iq2_xs", Some(alpha)));
            paddock.until_in_flight("iq2_xs", 2).await;
            sent.push(send_naming(&paddock, "iq2_xs", Some(alpha)));
            until_waiting(&paddock, 1).await;

            let told = reasons(&paddock).await;
            assert_eq!(told.len(), 1, "{told:?}");
            assert!(told[0].contains("alpha"), "{told:?}");
            assert!(!told[0].contains("beta"), "{told:?}");
            finish(release, sent).await;
        },
    )
    .await;
}

#[tokio::test]
async fn a_header_naming_a_busy_lease_waits_for_that_turn_though_another_is_free() {
    let (base, release, _seen) = fake_held().await;
    let models = small("iq2_xs", &base, "sequences = 2");
    with_paddock(
        paddock_config(&models),
        FakeShepherd::new(),
        |paddock| async move {
            let (_alpha, _alpha_events) =
                hold(&paddock, "mac-sessions", "iq2_xs", "alpha", false).await;
            let (beta, _beta_events) =
                hold(&paddock, "mac-sessions", "iq2_xs", "beta", false).await;

            let mut sent = vec![send_naming(&paddock, "iq2_xs", Some(beta))];
            paddock.until_in_flight("iq2_xs", 1).await;
            sent.push(send_naming(&paddock, "iq2_xs", Some(beta)));
            until_waiting(&paddock, 1).await;

            let told = reasons(&paddock).await;
            assert!(
                told[0].contains("beta") && !told[0].contains("alpha"),
                "{told:?}"
            );
            let (_, body) = json_of(get(&paddock, "/paddock/status").await).await;
            assert_eq!(body["waiters"][0]["lease"], render_id(beta), "{body}");
            assert_eq!(paddock.in_flight("iq2_xs").await, Some(1));
            finish(release, sent).await;
        },
    )
    .await;
}

#[tokio::test]
async fn a_header_naming_another_clients_lease_falls_through() {
    let (base, release, _seen) = fake_held().await;
    let models = small("iq2_xs", &base, "sequences = 1");
    with_paddock(
        two_clients(&models),
        FakeShepherd::new(),
        |paddock| async move {
            let (bench, _bench_events) = hold(&paddock, "bench-01", "iq2_xs", "bench", false).await;

            let mut sent = vec![send_naming(&paddock, "iq2_xs", Some(bench))];
            paddock.until_in_flight("iq2_xs", 1).await;
            sent.push(send_naming(&paddock, "iq2_xs", Some(bench)));
            until_waiting(&paddock, 1).await;

            let told = reasons(&paddock).await;
            assert!(
                told[0].contains("request turn") && !told[0].contains("bench"),
                "{told:?}"
            );
            finish(release, sent).await;
        },
    )
    .await;
}

#[tokio::test]
async fn a_header_naming_a_lease_on_another_model_falls_through() {
    let (base, release, _seen) = fake_held().await;
    let models = small("iq2_xs", &base, "sequences = 1") + &small("iq3_s", &base, "sequences = 1");
    with_paddock(
        paddock_config(&models),
        FakeShepherd::new(),
        |paddock| async move {
            let (other, _other_events) =
                hold(&paddock, "mac-sessions", "iq3_s", "elsewhere", false).await;

            let mut sent = vec![send_naming(&paddock, "iq2_xs", Some(other))];
            paddock.until_in_flight("iq2_xs", 1).await;
            sent.push(send_naming(&paddock, "iq2_xs", Some(other)));
            until_waiting(&paddock, 1).await;

            let told = reasons(&paddock).await;
            assert!(
                told[0].contains("iq2_xs request turn") && !told[0].contains("elsewhere"),
                "{told:?}"
            );
            finish(release, sent).await;
        },
    )
    .await;
}

#[tokio::test]
async fn a_header_naming_an_ended_lease_falls_through_and_the_request_is_served() {
    let (base, release, _seen) = fake_held().await;
    let models = small("iq2_xs", &base, "sequences = 1");
    with_paddock(
        paddock_config(&models),
        FakeShepherd::new(),
        |paddock| async move {
            let (ended, _events) = hold(&paddock, "mac-sessions", "iq2_xs", "ended", false).await;
            paddock
                .engine
                .release("mac-sessions".into(), ended)
                .await
                .expect("released");

            let mut sent = vec![send_naming(&paddock, "iq2_xs", Some(ended))];
            paddock.until_in_flight("iq2_xs", 1).await;
            sent.push(send_naming(&paddock, "iq2_xs", Some(ended)));
            until_waiting(&paddock, 1).await;

            let told = reasons(&paddock).await;
            assert!(told[0].contains("request turn"), "{told:?}");
            finish(release, sent).await;
        },
    )
    .await;
}

#[tokio::test]
async fn a_header_naming_a_reclaimable_lease_falls_through_to_the_request_turn() {
    let (base, release, _seen) = fake_held().await;
    let models = small("iq2_xs", &base, "sequences = 1");
    with_paddock(
        paddock_config(&models),
        FakeShepherd::new(),
        |paddock| async move {
            let (spare, _events) = hold(&paddock, "mac-sessions", "iq2_xs", "spare", true).await;

            let mut sent = vec![send_naming(&paddock, "iq2_xs", Some(spare))];
            paddock.until_in_flight("iq2_xs", 1).await;
            sent.push(send_naming(&paddock, "iq2_xs", Some(spare)));
            until_waiting(&paddock, 1).await;

            let told = reasons(&paddock).await;
            assert!(told[0].contains("request turn"), "{told:?}");
            finish(release, sent).await;
        },
    )
    .await;
}

#[tokio::test]
async fn a_malformed_lease_header_is_400_bad_lease() {
    for lease in ["l1", "L01", "1", "L", "L-1", "", "L99999999999999999999999"] {
        assert_eq!(
            refused_at_once(r#"{"model":"iq2_xs"}"#, &[("X-Paddock-Lease", lease)]).await,
            (400, json!({"error": "bad_lease"})),
            "{lease:?}"
        );
    }
}

#[tokio::test]
async fn the_lease_header_does_not_reach_the_backend() {
    let (base, release, seen) = fake_held().await;
    let models = small("iq2_xs", &base, "sequences = 1");
    with_paddock(
        paddock_config(&models),
        FakeShepherd::new(),
        |paddock| async move {
            let (alpha, _events) = hold(&paddock, "mac-sessions", "iq2_xs", "alpha", false).await;

            let sent = vec![send_naming(&paddock, "iq2_xs", Some(alpha))];
            paddock.until_in_flight("iq2_xs", 1).await;
            finish(release, sent).await;
        },
    )
    .await;

    let seen = seen.lock().expect("seen lock");
    assert_eq!(seen.len(), 1);
    assert!(seen[0].get("x-paddock-lease").is_none(), "{:?}", seen[0]);
}
