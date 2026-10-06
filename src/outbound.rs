//! The one HTTP client setup every request the dog sends goes through.

use reqwest::redirect::Policy;

/// A client that follows no redirect and uses no proxy
///
/// Requests carry a model's key and a client's prompt. A redirect would
/// take both to a url the dog never checked, and a proxy from the
/// environment would see both. A backend's redirect is passed to the
/// client as it is.
///
/// # Panics
/// If reqwest cannot build a client, which without TLS has no cause here.
#[track_caller]
pub(crate) fn http_client() -> reqwest::Client {
    match reqwest::Client::builder()
        .redirect(Policy::none())
        .no_proxy()
        .build()
    {
        Ok(client) => client,
        Err(err) => panic!("building the HTTP client failed: {err}"),
    }
}
