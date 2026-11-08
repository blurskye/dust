//! Bounded HTTPS requests to manifest-approved hosts, off the editor thread.

use super::PluginLoadId;
use super::component::component_api::dust::plugin::host as wit;
use std::collections::BTreeSet;
use std::io::Read;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::Duration;

struct Request {
    plugin: String,
    load: PluginLoadId,
    token: u64,
    request: wit::HttpRequest,
    hosts: BTreeSet<String>,
}

pub(crate) struct HttpReply {
    pub(crate) plugin: String,
    pub(crate) load: PluginLoadId,
    pub(crate) handler: String,
    pub(crate) response: wit::HttpResponse,
}

pub(crate) struct PluginHttp {
    sender: SyncSender<Request>,
    replies: Receiver<HttpReply>,
}

impl PluginHttp {
    pub(crate) fn start() -> Self {
        let (sender, requests) = mpsc::sync_channel::<Request>(16);
        let (replies, receiver) = mpsc::sync_channel(16);
        std::thread::Builder::new()
            .name("dust-plugin-http".into())
            .spawn(move || {
                while let Ok(request) = requests.recv() {
                    let mut response = wit::HttpResponse {
                        request: request.token,
                        status: 0,
                        headers: Vec::new(),
                        body: Vec::new(),
                        error: None,
                    };
                    if let Err(error) = fetch(&request.request, &request.hosts, &mut response) {
                        response.error = Some(error);
                    }
                    if replies
                        .send(HttpReply {
                            plugin: request.plugin,
                            load: request.load,
                            handler: request.request.handler,
                            response,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("plugin HTTP worker");
        Self {
            sender,
            replies: receiver,
        }
    }

    pub(crate) fn request(
        &self,
        plugin: String,
        load: PluginLoadId,
        token: u64,
        request: wit::HttpRequest,
        hosts: BTreeSet<String>,
    ) -> Result<(), String> {
        checked_url(&request.url, &hosts)?;
        self.sender
            .try_send(Request {
                plugin,
                load,
                token,
                request,
                hosts,
            })
            .map_err(|_| "plugin HTTP queue is full or stopped".into())
    }

    pub(crate) fn poll(&self) -> Vec<HttpReply> {
        self.replies.try_iter().collect()
    }
}

fn checked_url(url: &str, hosts: &BTreeSet<String>) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(url).map_err(|error| error.to_string())?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|port| port != 443)
        || url.host_str().is_none_or(|host| !hosts.contains(host))
    {
        return Err("HTTPS host is not granted".into());
    }
    Ok(url)
}

fn fetch(
    request: &wit::HttpRequest,
    hosts: &BTreeSet<String>,
    response: &mut wit::HttpResponse,
) -> Result<(), String> {
    let url = checked_url(&request.url, hosts)?;
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| error.to_string())?;
    let method = reqwest::Method::from_bytes(request.method.as_bytes())
        .map_err(|error| error.to_string())?;
    let mut builder = client.request(method, url).body(request.body.clone());
    for (name, value) in &request.headers {
        if matches!(
            name.to_ascii_lowercase().as_str(),
            "host" | "connection" | "content-length" | "transfer-encoding"
        ) {
            return Err("HTTP routing header is host-owned".into());
        }
        builder = builder.header(name, value);
    }
    let reply = builder.send().map_err(|error| error.to_string())?;
    response.status = reply.status().as_u16();
    for (name, value) in reply.headers().iter().take(64) {
        response.headers.push((
            name.to_string(),
            value
                .to_str()
                .map_err(|error| error.to_string())?
                .chars()
                .take(4096)
                .collect(),
        ));
    }
    reply
        .take(1024 * 1024 + 1)
        .read_to_end(&mut response.body)
        .map_err(|error| error.to_string())?;
    if response.body.len() > 1024 * 1024 {
        response.body.clear();
        return Err("HTTP response byte limit exceeded".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn http_checks_scheme_host_credentials_and_port_before_queueing() {
        let hosts = BTreeSet::from(["example.com".into()]);
        assert!(checked_url("https://example.com/path", &hosts).is_ok());
        for url in [
            "http://example.com",
            "https://other.com",
            "https://user@example.com",
            "https://example.com:444",
            "https://example.com.evil.test",
        ] {
            assert!(checked_url(url, &hosts).is_err(), "{url}");
        }
    }
}
