// SPDX-License-Identifier: MIT

//! Asynchronous HTTP and WebSocket transport for a document. IO workers only
//! send owned messages and never access a V8 isolate.

mod socket;

use std::{
    collections::HashMap,
    sync::{Arc, OnceLock, mpsc::Sender},
    time::Duration,
};

use reqwest::{Client, Method, header};
use tokio::{runtime::Runtime, sync::mpsc, task::JoinHandle};
use url::Url;

const MAX_REDIRECTS: usize = 20;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Default)]
pub struct Allowlist {
    hosts: Vec<(Url, Option<u16>)>,
    origins: Vec<Url>,
    dev: Option<Url>,
}

impl Allowlist {
    pub fn new(entries: &[String], dev: Option<Url>) -> Self {
        let mut policy = Self {
            dev,
            ..Self::default()
        };
        for entry in entries {
            if entry.contains("://") {
                if let Ok(url) = Url::parse(entry)
                    && valid(&url)
                    && url.path() == "/"
                    && url.query().is_none()
                    && url.fragment().is_none()
                {
                    policy.origins.push(url);
                }
            } else if !entry.contains(['/', '?', '#', '@', '\\'])
                && let Ok(url) = Url::parse(&format!("http://{entry}/"))
                && valid(&url)
            {
                // URL parsing drops an explicit default port. Preserve that
                // constraint so an allowlist entry ending in :80 stays exact.
                let authority = entry
                    .rsplit_once(']')
                    .map_or(entry.as_str(), |(_, suffix)| suffix);
                let port = authority
                    .rsplit_once(':')
                    .and_then(|(_, port)| port.parse().ok());
                policy.hosts.push((url, port));
            }
        }
        policy
    }

    pub fn permits(&self, url: &Url) -> bool {
        valid(url)
            && (self.hosts.iter().any(|(host, port)| {
                host.host() == url.host()
                    && port.is_none_or(|port| url.port_or_known_default() == Some(port))
            }) || self
                .origins
                .iter()
                .any(|origin| origin.origin() == url.origin())
                || self.dev.as_ref().is_some_and(|dev| {
                    let scheme = match url.scheme() {
                        "ws" => "http",
                        "wss" => "https",
                        scheme => scheme,
                    };
                    dev.scheme() == scheme
                        && dev.host() == url.host()
                        && dev.port_or_known_default() == url.port_or_known_default()
                }))
    }
}

fn valid(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https" | "ws" | "wss")
        && url.host().is_some()
        && url.username().is_empty()
        && url.password().is_none()
}

pub type Headers = Vec<(String, String)>;

#[derive(Debug)]
pub struct Request {
    pub url: Url,
    pub method: String,
    pub headers: Headers,
    pub body: Option<Vec<u8>>,
    pub redirect: Redirect,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Redirect {
    Follow,
    Error,
    Manual,
}

#[derive(Debug)]
pub struct Response {
    pub url: String,
    pub status: u16,
    pub status_text: String,
    pub headers: Headers,
    pub redirected: bool,
}

#[derive(Debug)]
pub enum Data {
    Text(String),
    Binary(Vec<u8>),
}

#[derive(Debug)]
pub enum Event {
    Headers(Response),
    Chunk(Vec<u8>),
    End,
    Open(String),
    Message(Data),
    Sent(usize),
    Close {
        code: u16,
        reason: String,
        clean: bool,
    },
    Error,
}

type EventWaker = dyn Fn() + Send + Sync;
type EventPacket = (u32, Event);

#[derive(Clone)]
struct Events {
    sender: Sender<EventPacket>,
    waker: Arc<OnceLock<Box<EventWaker>>>,
}

impl std::fmt::Debug for Events {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Events").finish_non_exhaustive()
    }
}

impl Events {
    fn send(&self, event: EventPacket) -> Result<(), std::sync::mpsc::SendError<EventPacket>> {
        self.sender.send(event)?;
        if let Some(wake) = self.waker.get() {
            wake();
        }
        Ok(())
    }
}

#[derive(Debug)]
enum Command {
    Credit(usize),
    Send(Data),
    Close(u16, String),
}

#[derive(Debug)]
struct Transfer {
    commands: mpsc::UnboundedSender<Command>,
    worker: JoinHandle<()>,
}

#[derive(Debug)]
pub struct Transport {
    policy: Allowlist,
    events: Events,
    transfers: HashMap<u32, Transfer>,
}

impl Transport {
    pub fn new(policy: Allowlist, sender: Sender<(u32, Event)>) -> Self {
        Self {
            policy,
            events: Events {
                sender,
                waker: Arc::default(),
            },
            transfers: HashMap::new(),
        }
    }

    /// Wake the consumer after publishing a packet, including transfers
    /// started before the callback was registered.
    pub fn set_waker(&self, wake: impl Fn() + Send + Sync + 'static) {
        let _ = self.events.waker.set(Box::new(wake));
    }

    pub fn active(&self) -> bool {
        !self.transfers.is_empty()
    }

    pub fn fetch(&mut self, id: u32, request: Request) {
        let policy = self.policy.clone();
        self.start(id, |events, commands| async move {
            if fetch(policy, request, &events, id, commands).await.is_err() {
                let _ = events.send((id, Event::Error));
            }
        });
    }

    pub fn socket(&mut self, id: u32, url: Url, origin: String, protocols: Vec<String>) {
        let policy = self.policy.clone();
        self.start(id, |events, commands| async move {
            if !policy.permits(&url)
                || socket::run(url, origin, protocols, &events, id, commands)
                    .await
                    .is_err()
            {
                let _ = events.send((id, Event::Error));
                let _ = events.send((
                    id,
                    Event::Close {
                        code: 1006,
                        reason: String::new(),
                        clean: false,
                    },
                ));
            }
        });
    }

    fn start<F>(&mut self, id: u32, run: impl FnOnce(Events, mpsc::UnboundedReceiver<Command>) -> F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        if let Ok(runtime) = runtime() {
            let (commands, receiver) = mpsc::unbounded_channel();
            let worker = runtime.spawn(run(self.events.clone(), receiver));
            self.transfers.insert(id, Transfer { commands, worker });
        } else {
            let _ = self.events.send((id, Event::Error));
        }
    }

    pub fn credit(&self, id: u32, bytes: usize) {
        self.command(id, Command::Credit(bytes));
    }

    pub fn send(&self, id: u32, data: Data) {
        self.command(id, Command::Send(data));
    }

    pub fn close(&self, id: u32, code: u16, reason: String) {
        self.command(id, Command::Close(code, reason));
    }

    fn command(&self, id: u32, command: Command) {
        if let Some(transfer) = self.transfers.get(&id) {
            let _ = transfer.commands.send(command);
        }
    }

    pub fn cancel(&mut self, id: u32) {
        if let Some(transfer) = self.transfers.remove(&id) {
            transfer.worker.abort();
        }
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        for transfer in self.transfers.values() {
            transfer.worker.abort();
        }
    }
}

fn runtime() -> Result<&'static Runtime, ()> {
    static IO: OnceLock<Option<Runtime>> = OnceLock::new();
    IO.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("squark-net")
            .enable_all()
            .build()
            .ok()
    })
    .as_ref()
    .ok_or(())
}

fn client() -> Result<&'static Client, ()> {
    static CLIENT: OnceLock<Option<Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            Client::builder()
                .no_proxy()
                .connect_timeout(CONNECT_TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .ok()
        })
        .as_ref()
        .ok_or(())
}

async fn fetch(
    policy: Allowlist,
    mut request: Request,
    events: &Events,
    id: u32,
    mut commands: mpsc::UnboundedReceiver<Command>,
) -> Result<(), ()> {
    let client = client()?;
    let mut method = Method::from_bytes(request.method.as_bytes()).map_err(|_| ())?;
    let mut headers = header::HeaderMap::new();
    for (name, value) in request.headers {
        let name = header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| ())?;
        if matches!(
            name,
            header::HOST
                | header::CONTENT_LENGTH
                | header::TRANSFER_ENCODING
                | header::CONNECTION
                | header::UPGRADE
                | header::PROXY_AUTHORIZATION
                | header::COOKIE
        ) {
            continue;
        }
        let bytes = value
            .chars()
            .map(|ch| u8::try_from(u32::from(ch)))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| ())?;
        headers.append(
            name,
            header::HeaderValue::from_bytes(&bytes).map_err(|_| ())?,
        );
    }
    for hop in 0..=MAX_REDIRECTS {
        if !matches!(request.url.scheme(), "http" | "https") || !policy.permits(&request.url) {
            return Err(());
        }
        request.url.set_fragment(None);
        let mut outgoing = client
            .request(method.clone(), request.url.clone())
            .headers(headers.clone());
        if let Some(body) = &request.body {
            outgoing = outgoing.body(body.clone());
        }
        let mut response = outgoing.send().await.map_err(|_| ())?;
        let status = response.status();
        if matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
            && let Some(location) = response.headers().get(header::LOCATION)
        {
            match request.redirect {
                Redirect::Error => return Err(()),
                Redirect::Manual => {
                    events
                        .send((
                            id,
                            Event::Headers(Response {
                                url: String::new(),
                                status: 0,
                                status_text: String::new(),
                                headers: Vec::new(),
                                redirected: false,
                            }),
                        ))
                        .map_err(|_| ())?;
                    return events.send((id, Event::End)).map_err(|_| ());
                }
                Redirect::Follow => {}
            }
            let next = request
                .url
                .join(location.to_str().map_err(|_| ())?)
                .map_err(|_| ())?;
            if next.origin() != request.url.origin() {
                headers.remove(header::AUTHORIZATION);
                headers.remove(header::COOKIE);
            }
            if (matches!(status.as_u16(), 301 | 302) && method == Method::POST)
                || (status.as_u16() == 303 && !matches!(method, Method::GET | Method::HEAD))
            {
                method = Method::GET;
                request.body = None;
                for name in [
                    header::CONTENT_TYPE,
                    header::CONTENT_ENCODING,
                    header::CONTENT_LANGUAGE,
                    header::CONTENT_LOCATION,
                ] {
                    headers.remove(name);
                }
            }
            request.url = next;
            continue;
        }
        let headers = response
            .headers()
            .iter()
            .filter(|(name, _)| !matches!(**name, header::SET_COOKIE))
            .map(|(name, value)| {
                (
                    name.to_string(),
                    value.as_bytes().iter().copied().map(char::from).collect(),
                )
            })
            .collect();
        events
            .send((
                id,
                Event::Headers(Response {
                    url: request.url.to_string(),
                    status: status.as_u16(),
                    status_text: status.canonical_reason().unwrap_or_default().to_owned(),
                    headers,
                    redirected: hop != 0,
                }),
            ))
            .map_err(|_| ())?;
        if method != Method::HEAD && !matches!(status.as_u16(), 204 | 205 | 304) {
            let mut chunk = Default::default();
            while let Some(Command::Credit(bytes)) = commands.recv().await {
                if bytes == 0 {
                    continue;
                }
                if !send_chunk(&mut response, &mut chunk, bytes, events, id).await? {
                    break;
                }
            }
        }
        return events.send((id, Event::End)).map_err(|_| ());
    }
    Err(())
}

async fn send_chunk(
    response: &mut reqwest::Response,
    chunk: &mut bytes::Bytes,
    credit: usize,
    events: &Events,
    id: u32,
) -> Result<bool, ()> {
    while chunk.is_empty() {
        let Some(next) = response.chunk().await.map_err(|_| ())? else {
            return Ok(false);
        };
        *chunk = next;
    }
    let bytes = chunk.split_to(credit.min(chunk.len())).to_vec();
    events.send((id, Event::Chunk(bytes))).map_err(|_| ())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn egress_policy_requires_exact_hosts_and_respects_explicit_origins() {
        let dev = Url::parse("http://localhost:5173/index.html").unwrap();
        let policy = Allowlist::new(
            &[
                "api.acme.dev".into(),
                "wss://events.acme.dev".into(),
                "limited.acme.dev:80".into(),
            ],
            Some(dev),
        );
        for allowed in [
            "https://api.acme.dev/v1",
            "wss://events.acme.dev/stream",
            "http://limited.acme.dev/x",
            "ws://localhost:5173/socket",
        ] {
            assert!(policy.permits(&Url::parse(allowed).unwrap()), "{allowed}");
        }
        for denied in [
            "https://api.acme.dev.evil.test",
            "https://sub.api.acme.dev",
            "https://events.acme.dev",
            "wss://events.acme.dev:1234",
            "https://limited.acme.dev",
            "ws://localhost:5174",
            "http://user@api.acme.dev/",
            "ftp://api.acme.dev/file",
        ] {
            assert!(!policy.permits(&Url::parse(denied).unwrap()), "{denied}");
        }
    }
}
