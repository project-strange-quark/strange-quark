// SPDX-License-Identifier: MIT

use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use tokio::{sync::mpsc, time};
use tokio_tungstenite::{
    Connector, connect_async_tls_with_config,
    tungstenite::{Message, client::IntoClientRequest, protocol::CloseFrame},
};
use url::Url;

use super::{CONNECT_TIMEOUT, Command, Data, Event, Events};

fn connector() -> Result<Connector, ()> {
    static CONFIG: OnceLock<Option<Arc<rustls::ClientConfig>>> = OnceLock::new();
    let config = CONFIG
        .get_or_init(|| {
            let mut roots = rustls::RootCertStore::empty();
            roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
            let config = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .ok()?
            .with_root_certificates(roots)
            .with_no_client_auth();
            Some(Arc::new(config))
        })
        .as_ref()
        .ok_or(())?;
    Ok(Connector::Rustls(Arc::clone(config)))
}

pub(super) async fn run(
    url: Url,
    origin: String,
    protocols: Vec<String>,
    events: &Events,
    id: u32,
    mut commands: mpsc::UnboundedReceiver<Command>,
) -> Result<(), ()> {
    let mut request = url.as_str().into_client_request().map_err(|_| ())?;
    request
        .headers_mut()
        .insert("origin", origin.parse().map_err(|_| ())?);
    if !protocols.is_empty() {
        request.headers_mut().insert(
            "sec-websocket-protocol",
            protocols.join(", ").parse().map_err(|_| ())?,
        );
    }
    let connection = connect_async_tls_with_config(request, None, false, Some(connector()?));
    let (mut socket, response) = tokio::select! {
        result = time::timeout(CONNECT_TIMEOUT, connection) => result.map_err(|_| ())?.map_err(|_| ())?,
        _ = commands.recv() => return Err(()),
    };
    let protocol = response
        .headers()
        .get("sec-websocket-protocol")
        .map(|value| value.to_str().map(str::to_owned))
        .transpose()
        .map_err(|_| ())?
        .unwrap_or_default();
    if !protocol.is_empty() && !protocols.contains(&protocol) {
        return Err(());
    }
    events.send((id, Event::Open(protocol))).map_err(|_| ())?;
    let mut closing = None;
    loop {
        tokio::select! {
            incoming = socket.next() => match incoming {
                Some(Ok(Message::Text(value))) => events.send((id, Event::Message(Data::Text(value.to_string())))).map_err(|_| ())?,
                Some(Ok(Message::Binary(value))) => events.send((id, Event::Message(Data::Binary(value.to_vec())))).map_err(|_| ())?,
                Some(Ok(Message::Ping(_))) => socket.flush().await.map_err(|_| ())?,
                Some(Ok(Message::Close(frame))) => {
                    let _ = socket.flush().await;
                    let (code, reason) = frame.map_or_else(|| (1005, String::new()), |frame| (frame.code.into(), frame.reason.to_string()));
                    return events.send((id, Event::Close { code, reason, clean: true })).map_err(|_| ());
                }
                Some(Ok(_)) => {}
                _ => return Err(()),
            },
            command = commands.recv() => match command {
                Some(Command::Send(data)) if closing.is_none() => {
                    let message = match data {
                        Data::Text(value) => Message::Text(value.into()),
                        Data::Binary(value) => Message::Binary(value.into()),
                    };
                    let bytes = message.len();
                    socket.send(message).await.map_err(|_| ())?;
                    events.send((id, Event::Sent(bytes))).map_err(|_| ())?;
                }
                Some(Command::Close(code, reason)) if closing.is_none() => {
                    socket.close((code != 1005).then(|| CloseFrame { code: code.into(), reason: reason.into() })).await.map_err(|_| ())?;
                    closing = Some(time::Instant::now() + Duration::from_secs(5));
                }
                Some(_) => {}
                None => return Ok(()),
            },
            () = async {
                match closing {
                    Some(deadline) => time::sleep_until(deadline).await,
                    None => std::future::pending().await,
                }
            } => return Err(()),
        }
    }
}
