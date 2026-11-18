//! Conditional GETs for every REST read.
//! `ForgeContext::send` asks here before a GET goes out: when an earlier answer to the same
//! request carried an `ETag`, `If-None-Match` is attached, and a `304` is turned back into that
//! earlier answer. GitHub does not count a 304 against the rate limit, and the forge sends no
//! body, so a refresh of something unchanged costs a header exchange. The store is per process
//! and bounded; a body is kept only when it is small enough to be worth it.

use crate::transport::ApiRequest;
use crate::transport::ApiResponse;
use crate::transport::Method;
use crate::transport::Target;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

const ENTRIES: usize = 512;
const BODY_LIMIT: usize = 2 * 1024 * 1024;

struct Stored {
    etag: String,
    response: ApiResponse,
    used: u64,
}

#[derive(Default)]
struct Store {
    clock: u64,
    entries: HashMap<String, Stored>,
}

fn store() -> &'static Mutex<Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(Store::default()))
}

static REVALIDATED: AtomicU64 = AtomicU64::new(0);

/// How many answers this process served from a `304` (tests, `dusty-git forge stats`).
pub fn revalidated() -> u64 {
    REVALIDATED.load(Ordering::Relaxed)
}

/// The store key of a request that may be revalidated: REST GETs whose caller did not set its
/// own `If-None-Match` (a read that turns a 304 into `Fetched::NotModified` does).
pub fn key(api_base: &str, request: &ApiRequest) -> Option<String> {
    if request.method != Method::Get
        || !matches!(request.target, Target::Rest(_) | Target::Absolute(_))
        || request
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("if-none-match"))
    {
        return None;
    }
    Some(format!(
        "{api_base}|{:?}|{:?}|{:?}",
        request.target, request.query, request.accept
    ))
}

/// Attach `If-None-Match` when an answer is stored.
pub fn prepare(key: &str, request: &mut ApiRequest) {
    let store = store().lock().unwrap_or_else(|error| error.into_inner());
    if let Some(stored) = store.entries.get(key) {
        request
            .headers
            .push(("If-None-Match".into(), stored.etag.clone()));
    }
}

/// Turn a 304 into the stored answer; remember a fresh one that carries an ETag.
pub fn settle(key: &str, response: ApiResponse) -> ApiResponse {
    let mut store = store().lock().unwrap_or_else(|error| error.into_inner());
    store.clock += 1;
    let clock = store.clock;
    if response.status == 304 {
        if let Some(stored) = store.entries.get_mut(key) {
            stored.used = clock;
            REVALIDATED.fetch_add(1, Ordering::Relaxed);
            let mut answer = stored.response.clone();
            // Rate-limit headers and the like are the new response's.
            for (name, value) in response.headers {
                if let Some(slot) = answer.headers.iter_mut().find(|(known, _)| *known == name) {
                    slot.1 = value;
                } else {
                    answer.headers.push((name, value));
                }
            }
            return answer;
        }
        return response;
    }
    match response.header("etag") {
        Some(etag) if response.status == 200 && response.body.len() <= BODY_LIMIT => {
            let etag = etag.to_owned();
            if store.entries.len() >= ENTRIES && !store.entries.contains_key(key) {
                let oldest = store
                    .entries
                    .iter()
                    .min_by_key(|(_, stored)| stored.used)
                    .map(|(key, _)| key.clone());
                if let Some(oldest) = oldest {
                    store.entries.remove(&oldest);
                }
            }
            store.entries.insert(
                key.to_owned(),
                Stored {
                    etag,
                    response: response.clone(),
                    used: clock,
                },
            );
        }
        _ if !response.success() => {}
        _ => {
            store.entries.remove(key);
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, etag: Option<&str>, body: &str) -> ApiResponse {
        ApiResponse {
            status,
            headers: etag
                .map(|etag| vec![("etag".to_owned(), etag.to_owned())])
                .unwrap_or_default(),
            body: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn a_304_returns_the_stored_answer() {
        let request = ApiRequest::get("repos/o/r/actions/runs").query("per_page", 20);
        let key = key("https://etag.test/api/v3", &request).unwrap();
        let mut first = request.clone();
        prepare(&key, &mut first);
        assert!(first.headers.is_empty());
        settle(&key, response(200, Some("\"a\""), "[1]"));
        let mut second = request.clone();
        prepare(&key, &mut second);
        assert_eq!(
            second.headers,
            [("If-None-Match".to_owned(), "\"a\"".to_owned())]
        );
        let answer = settle(&key, response(304, None, ""));
        assert_eq!((answer.status, answer.body.as_slice()), (200, &b"[1]"[..]));
        assert!(super::key("x", &ApiRequest::get("p").header("If-None-Match", "\"z\"")).is_none());
        assert!(
            super::key(
                "x",
                &ApiRequest::graphql("query A { a }", serde_json::json!({}))
            )
            .is_none()
        );
    }
}
