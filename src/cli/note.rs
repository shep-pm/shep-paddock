//! `shep paddock note`: tell the dog the lease in `$PADDOCK_LEASE` is still in use.

use std::{io::Write, time::Duration};

use reqwest::{Method, StatusCode};
use serde_json::json;

use super::{Link, USAGE_EXIT, say, unreachable};
use crate::outbound::http_client;

// The dog answers a note from memory; ten seconds is a dog that is not answering.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// `id` as one path segment: every byte but an unreserved one becomes `%XX`
pub(super) fn segment(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for byte in id.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Sends `text` as a progress note on `lease`, returning the exit code
///
/// No lease is a usage error and nothing is sent. A note on a lease the dog no longer has, or
/// any answer but 204, exits 1.
pub(crate) async fn note(
    link: &Link,
    lease: Option<String>,
    text: &str,
    err: &mut impl Write,
) -> u8 {
    let Some(lease) = lease.filter(|lease| !lease.is_empty()) else {
        say(
            err,
            "$PADDOCK_LEASE is not set. `run` sets it for its command; note works inside one.",
        );
        return USAGE_EXIT;
    };
    let client = http_client();
    let sent = link
        .request(
            &client,
            Method::PUT,
            &format!("/paddock/leases/{}", segment(&lease)),
        )
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(json!({ "note": text }).to_string())
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await;
    let response = match sent {
        Ok(response) => response,
        Err(failure) => {
            unreachable(err, link, failure);
            return 1;
        }
    };
    let code = response.status();
    if code == StatusCode::NO_CONTENT {
        return 0;
    }
    if code == StatusCode::NOT_FOUND {
        say(err, format_args!("lease {lease} is gone"));
        return 1;
    }
    let body = response.text().await.unwrap_or_default();
    say(err, format_args!("the dog answered {code}: {body}"));
    1
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use tokio::time::timeout;

    use super::{Duration, note};
    use crate::{cli::Link, test_support::fake_http};

    // Real time, because the fake dog is a real loopback socket.
    const LIMIT: Duration = Duration::from_secs(10);

    fn link(url: String) -> Link {
        Link {
            url,
            key: "k-bench".to_owned(),
            retry: Duration::from_millis(10),
            silence: Duration::from_secs(45),
        }
    }

    #[tokio::test]
    async fn note_puts_the_text_on_the_lease_in_paddock_lease() {
        let (url, dog) = fake_http(vec![("PUT", "/paddock/leases/L7", vec![(204, "")])]);
        let mut err = Vec::new();
        let code = timeout(
            LIMIT,
            note(&link(url), Some("L7".to_owned()), "step 412/900", &mut err),
        )
        .await
        .expect("finishes");
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        assert!(err.is_empty());
        let seen = dog.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].authorization.as_deref(), Some("Bearer k-bench"));
        let body: Value = serde_json::from_str(&seen[0].body).expect("a JSON body");
        assert_eq!(body, json!({ "note": "step 412/900" }));
    }

    #[tokio::test]
    async fn a_lease_id_with_path_characters_stays_one_path_segment() {
        let (url, dog) = fake_http(vec![(
            "PUT",
            "/paddock/leases/a%2Fb%3Fc%20d",
            vec![(204, "")],
        )]);
        let mut err = Vec::new();
        let lease = Some("a/b?c d".to_owned());
        let code = timeout(LIMIT, note(&link(url), lease, "x", &mut err))
            .await
            .expect("finishes");
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        assert_eq!(dog.seen().len(), 1);
    }

    #[tokio::test]
    async fn note_without_a_lease_exits_2_and_sends_nothing() {
        let (url, dog) = fake_http(vec![]);
        let mut err = Vec::new();
        let code = timeout(LIMIT, note(&link(url), None, "x", &mut err))
            .await
            .expect("finishes");
        assert_eq!(code, 2);
        assert!(String::from_utf8_lossy(&err).contains("$PADDOCK_LEASE is not set"));
        assert!(dog.seen().is_empty());
    }

    #[tokio::test]
    async fn a_note_on_a_gone_lease_exits_1() {
        let (url, _dog) = fake_http(vec![(
            "PUT",
            "/paddock/leases/L7",
            vec![(404, r#"{"error":"not_found"}"#)],
        )]);
        let mut err = Vec::new();
        let code = timeout(
            LIMIT,
            note(&link(url), Some("L7".to_owned()), "x", &mut err),
        )
        .await
        .expect("finishes");
        assert_eq!(code, 1);
        assert_eq!(String::from_utf8_lossy(&err), "paddock: lease L7 is gone\n");
    }

    #[tokio::test]
    async fn a_refused_note_exits_1_with_the_dogs_answer() {
        let (url, _dog) = fake_http(vec![(
            "PUT",
            "/paddock/leases/L7",
            vec![(400, r#"{"error":"note_too_long"}"#)],
        )]);
        let mut err = Vec::new();
        let code = timeout(
            LIMIT,
            note(&link(url), Some("L7".to_owned()), "x", &mut err),
        )
        .await
        .expect("finishes");
        assert_eq!(code, 1);
        let said = String::from_utf8_lossy(&err);
        assert!(
            said.contains("400") && said.contains("note_too_long"),
            "{said}"
        );
    }

    #[tokio::test]
    async fn the_dogs_answer_cannot_write_escape_codes() {
        let (url, _dog) = fake_http(vec![(
            "PUT",
            "/paddock/leases/L7",
            vec![(500, "boom \u{1b}[2J\u{7}")],
        )]);
        let mut err = Vec::new();
        let code = timeout(
            LIMIT,
            note(&link(url), Some("L7".to_owned()), "x", &mut err),
        )
        .await
        .expect("finishes");
        assert_eq!(code, 1);
        let said = String::from_utf8_lossy(&err);
        assert!(said.contains("boom "), "{said}");
        assert!(
            !said.chars().any(|c| c.is_control() && c != '\n'),
            "{said:?}"
        );
    }
}
