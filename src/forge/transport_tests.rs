//! The bounded transport, proved against LOCAL fixtures. Nothing here reaches a network: every
//! address is a loopback listener this test binds itself, and the fixture RECORDS what it
//! receives, which is how a claim like "refused before transmission" becomes a measurement.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::transport::{ForgeResponse, ForgeTransport};
use super::*;

pub(super) fn response(status: &str, headers: &[(&str, &str)], body: &str) -> String {
    let mut reply = format!("HTTP/1.1 {status}\r\n");
    for (name, value) in headers {
        reply.push_str(&format!("{name}: {value}\r\n"));
    }
    reply.push_str(&format!("content-length: {}\r\nconnection: close\r\n\r\n", body.len()));
    reply.push_str(body);
    reply
}

/// A local fixture forge: it records every request it receives, so a test can ask what ARRIVED
/// rather than what an error message said.
pub(super) struct Fixture {
    pub(super) origin: ApiOrigin,
    port: u16,
    requests: Arc<Mutex<Vec<String>>>,
    handle: Option<JoinHandle<()>>,
}

impl Fixture {
    pub(super) fn serve(expected: usize, responder: impl Fn(usize, &str) -> String + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local address").port();
        listener.set_nonblocking(true).expect("nonblocking");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut served = 0;
            while served < expected && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buffer = [0u8; 8192];
                        let read = stream.read(&mut buffer).unwrap_or(0);
                        let head = String::from_utf8_lossy(&buffer[..read]).into_owned();
                        recorded.lock().expect("fixture lock").push(head.clone());
                        let reply = responder(served, &head);
                        let _ = stream.write_all(reply.as_bytes());
                        let _ = stream.flush();
                        served += 1;
                    }
                    Err(_) => thread::sleep(Duration::from_millis(2)),
                }
            }
        });
        Self {
            origin: ApiOrigin::fixture(&format!("http://127.0.0.1:{port}")).expect("fixture origin"),
            port,
            requests,
            handle: Some(handle),
        }
    }

    pub(super) fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    pub(super) fn requests(&self) -> Vec<String> {
        self.requests.lock().expect("fixture lock").clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn transport() -> ForgeTransport {
    ForgeTransport::with_limits(
        Duration::from_millis(500),
        Duration::from_millis(1500),
        MAX_REDIRECTS,
    )
    .expect("transport")
}

fn credential() -> Credential {
    Credential::new(CredentialSource::Store, "github", "ghp_supersecret")
}

fn header_present(request: &str, name: &str) -> bool {
    request
        .lines()
        .any(|line| line.to_ascii_lowercase().starts_with(&format!("{name}:")))
}

#[test]
fn a_request_that_would_leave_the_origin_is_refused_and_never_arrives() {
    // The origin's own listener, and a SECOND live fixture that records anything that reaches it.
    let origin_fixture = Fixture::serve(0, |_, _| response("200 OK", &[], ""));
    let outside = Fixture::serve(1, |_, _| response("200 OK", &[], "outside"));
    let origin = origin_fixture.origin.clone();
    let planned = PlannedRequest {
        address: outside.url("/escaped"),
        credential: Some(credential()),
    };

    assert_eq!(
        transport().send(Provider::GitHub, "GET", &planned, &origin, &[]),
        Err(ForgeRefusal::OriginEscape)
    );
    // THE MEASUREMENT: the other origin received NOTHING. Asserting the error alone would pass even
    // if the request had been sent first.
    assert!(
        outside.requests().is_empty(),
        "the request was refused BEFORE transmission: {:?}",
        outside.requests()
    );
    assert!(origin_fixture.requests().is_empty());
}

#[test]
fn a_cross_origin_redirect_never_carries_the_credential_to_the_next_listener() {
    let next = Fixture::serve(1, |_, _| response("200 OK", &[], "next"));
    let destination = next.url("/next");
    let first = Fixture::serve(1, move |_, _| {
        response("302 Found", &[("location", &destination)], "")
    });
    let origin = first.origin.clone();

    let planned = PlannedRequest {
        address: first.url("/start"),
        credential: Some(credential()),
    };
    let sent = transport()
        .send(Provider::GitHub, "GET", &planned, &origin, &["127.0.0.1"])
        .expect("the redirect is followed to an explicitly allowed host");
    assert_eq!(sent, ForgeResponse { status: 200, body: "next".into() });

    // The first hop DID carry it, so "never send a credential" cannot pass this test.
    assert!(
        first.requests()[0].to_ascii_lowercase().contains("ghp_supersecret"),
        "the first hop is authenticated"
    );
    // THE MEASUREMENT: the second listener saw NO Authorization header and no secret at all.
    let second = &next.requests()[0];
    assert!(
        !header_present(second, "authorization") && !second.contains("ghp_supersecret"),
        "the credential must not cross a host redirect: {second:?}"
    );
}

#[test]
fn a_same_origin_redirect_keeps_its_credential_and_an_admitted_request_transmits() {
    // The control on both sides: an implementation that dropped every credential, or refused
    // everything, must fail here.
    let fixture = Fixture::serve(2, |index, _| {
        if index == 0 {
            response("302 Found", &[("location", "/again")], "")
        } else {
            response("200 OK", &[], "arrived")
        }
    });
    let origin = fixture.origin.clone();
    let planned = PlannedRequest {
        address: fixture.url("/start"),
        credential: Some(credential()),
    };
    let sent = transport()
        .send(Provider::GitHub, "GET", &planned, &origin, &[])
        .expect("an admitted request transmits and its redirect stays inside the origin");
    assert_eq!(sent, ForgeResponse { status: 200, body: "arrived".into() });
    let requests = fixture.requests();
    assert_eq!(requests.len(), 2, "the control request really was transmitted");
    for request in &requests {
        assert!(
            header_present(request, "authorization"),
            "a same-origin hop keeps its credential: {request:?}"
        );
    }
}

#[test]
fn a_response_larger_than_the_bound_is_refused_while_it_is_read() {
    let body = "x".repeat(MAX_RESPONSE_BYTES + 1);
    let fixture = Fixture::serve(1, move |_, _| response("200 OK", &[], &body));
    let origin = fixture.origin.clone();
    let planned = PlannedRequest {
        address: fixture.url("/huge"),
        credential: None,
    };
    assert_eq!(
        transport().send(Provider::GitHub, "GET", &planned, &origin, &[]),
        Err(ForgeRefusal::ResponseTooLarge)
    );
}

#[test]
fn a_redirect_chain_beyond_its_bound_is_refused_rather_than_followed() {
    // Every hop redirects again: the chain must stop at the bound instead of walking forever.
    let fixture = Fixture::serve(MAX_REDIRECTS + 2, |_, head| {
        let path = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_owned();
        let next = format!("/{}", path.trim_start_matches('/').len() + 1);
        response("302 Found", &[("location", &next)], "")
    });
    let origin = fixture.origin.clone();
    let planned = PlannedRequest {
        address: fixture.url("/0"),
        credential: Some(credential()),
    };
    assert_eq!(
        transport().send(Provider::GitHub, "GET", &planned, &origin, &[]),
        Err(ForgeRefusal::RedirectLimit)
    );
}
