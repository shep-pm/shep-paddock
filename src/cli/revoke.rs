//! `shep paddock revoke`: end a lease, as an admin client.

use std::{io::Write, time::Duration};

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use super::{Link, note::segment, say, unreachable};
use crate::outbound::http_client;

// The dog answers a revoke from memory; ten seconds is a dog that is not answering.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Revokes lease `id`, giving `reason` when there is one, and returns the exit code
///
/// A `403` says this client is not an admin, or that the lease is held by a protected client, a
/// `404` that the lease is gone. Either, or any answer but `204`, exits 1.
pub(crate) async fn revoke(
    link: &Link,
    id: &str,
    reason: Option<&str>,
    err: &mut impl Write,
) -> u8 {
    let client = http_client();
    let path = format!("/paddock/leases/{}/revoke", segment(id));
    let mut request = link
        .request(&client, Method::POST, &path)
        .timeout(REQUEST_TIMEOUT);
    if let Some(reason) = reason {
        request = request
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(json!({ "reason": reason }).to_string());
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(failure) => {
            unreachable(err, link, failure);
            return 1;
        }
    };
    match response.status() {
        StatusCode::NO_CONTENT => 0,
        StatusCode::FORBIDDEN => {
            let body = response.text().await.unwrap_or_default();
            let protected =
                serde_json::from_str::<Value>(&body).is_ok_and(|body| body["error"] == "protected");
            if protected {
                say(
                    err,
                    format_args!("lease {id} is held by a protected client"),
                );
            } else {
                say(
                    err,
                    "this client is not an admin, so it cannot revoke a lease",
                );
            }
            1
        }
        StatusCode::NOT_FOUND => {
            say(err, format_args!("lease {id} is gone"));
            1
        }
        code => {
            let body = response.text().await.unwrap_or_default();
            say(err, format_args!("the dog answered {code}: {body}"));
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use tokio::time::timeout;

    use super::{Duration, revoke};
    use crate::{
        cli::{Command, Link, parse},
        test_support::fake_http,
    };

    // Real time, because the fake dog is a real loopback socket.
    const LIMIT: Duration = Duration::from_secs(10);

    fn link(url: String) -> Link {
        Link {
            url,
            key: "k-mac".to_owned(),
            retry: Duration::from_millis(10),
            silence: Duration::from_secs(45),
        }
    }

    async fn revoked(
        answer: (u16, &'static str),
        reason: Option<&str>,
    ) -> (u8, String, Option<String>) {
        let (url, dog) = fake_http(vec![("POST", "/paddock/leases/L12/revoke", vec![answer])]);
        let mut err = Vec::new();
        let code = timeout(LIMIT, revoke(&link(url), "L12", reason, &mut err))
            .await
            .expect("finishes");
        let body = dog.seen().first().map(|seen| seen.body.clone());
        (code, String::from_utf8_lossy(&err).into_owned(), body)
    }

    #[tokio::test]
    async fn revoke_posts_the_reason_to_the_leases_revoke_route() {
        let (code, said, body) = revoked((204, ""), Some("forgotten since Tuesday")).await;
        assert_eq!(code, 0, "{said}");
        let body: Value = serde_json::from_str(&body.expect("a request")).expect("JSON");
        assert_eq!(body, json!({ "reason": "forgotten since Tuesday" }));
    }

    #[tokio::test]
    async fn without_a_reason_the_body_is_empty() {
        let (code, _, body) = revoked((204, ""), None).await;
        assert_eq!((code, body.as_deref()), (0, Some("")));
    }

    #[tokio::test]
    async fn a_client_that_is_not_an_admin_is_told_so() {
        let (code, said, _) = revoked((403, r#"{"error":"forbidden"}"#), None).await;
        assert_eq!(code, 1);
        assert!(said.contains("not an admin"), "{said}");
    }

    #[tokio::test]
    async fn a_lease_of_a_protected_client_is_named_as_protected() {
        let (code, said, _) = revoked((403, r#"{"error":"protected"}"#), None).await;
        assert_eq!(code, 1);
        assert!(
            said.contains("lease L12 is held by a protected client"),
            "{said}"
        );
        assert!(!said.contains("not an admin"), "{said}");
    }

    #[tokio::test]
    async fn a_lease_that_is_gone_exits_1() {
        let (code, said, _) = revoked((404, r#"{"error":"not_found"}"#), None).await;
        assert_eq!(code, 1);
        assert!(said.contains("lease L12 is gone"), "{said}");
    }

    #[tokio::test]
    async fn an_unreachable_dog_is_named_without_the_credentials_in_its_url() {
        let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = closed.local_addr().expect("local addr").port();
        drop(closed);
        let url = format!("http://ops:hunter2@127.0.0.1:{port}");
        let mut err = Vec::new();
        let code = timeout(LIMIT, revoke(&link(url), "L12", None, &mut err))
            .await
            .expect("finishes");
        let said = String::from_utf8_lossy(&err);
        assert_eq!(code, 1);
        assert!(
            said.contains(&format!("cannot reach the dog at http://127.0.0.1:{port}")),
            "{said}"
        );
        assert!(!said.contains("hunter2"), "{said}");
    }

    #[test]
    fn revoke_takes_an_id_and_an_optional_reason() {
        let parsed = |words: &[&str]| parse(words.iter().copied());
        assert_eq!(
            parsed(&["revoke", "L12"]),
            Ok(Command::Revoke {
                id: "L12".to_owned(),
                reason: None
            })
        );
        assert_eq!(
            parsed(&["revoke", "L12", "--reason", "forgotten since Tuesday"]),
            Ok(Command::Revoke {
                id: "L12".to_owned(),
                reason: Some("forgotten since Tuesday".to_owned())
            })
        );
        let refused = |words: &[&str]| parsed(words).expect_err("refused").to_string();
        assert!(refused(&["revoke"]).contains("revoke takes the id"));
        assert!(refused(&["revoke", "L12", "L13"]).contains("given more than once"));
        assert!(refused(&["revoke", "L12", "--why", "x"]).contains("does not understand --why"));
    }
}
