//! `/api/kernels/{id}/channels`: one websocket per client, framed with the
//! v1 subprotocol when the client offers it and legacy JSON otherwise.

use crate::routes::{origin_allowed, HttpError};
use crate::write_observer::WriteEvent;
use crate::AppState;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use ganymede_api::ClientConnection;
use ganymede_protocol::ws::{legacy, v1};
use tokio::sync::broadcast;

#[derive(serde::Deserialize)]
pub struct ChannelsQuery {
    /// The client's session: what buffered output is kept for across a
    /// reconnect, as in `jupyter_server`.
    session_id: Option<String>,
}

pub async fn channels(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<ChannelsQuery>,
    headers: HeaderMap,
) -> Response {
    if !origin_allowed(&state, &headers) {
        return HttpError(403, "origin not allowed".into()).into_response();
    }
    let client = match state
        .kernels
        .connect_client_as(&id, query.session_id.as_deref())
    {
        Ok(c) => c,
        Err(e) => return HttpError::from(e).into_response(),
    };
    ws.protocols([v1::SUBPROTOCOL])
        .on_upgrade(move |socket| run(socket, client))
}

async fn run(mut socket: WebSocket, mut client: ClientConnection) {
    let use_v1 = socket.protocol().and_then(|p| p.to_str().ok()) == Some(v1::SUBPROTOCOL);
    tracing::debug!(
        kernel = client.kernel_id(),
        v1 = use_v1,
        "websocket connected"
    );
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                let decoded = match incoming {
                    Some(Ok(WsMessage::Binary(b))) => {
                        if use_v1 { v1::decode(&b) } else { legacy::decode_binary(&b) }
                    }
                    Some(Ok(WsMessage::Text(t))) => legacy::decode_text(t.as_str()),
                    Some(Ok(WsMessage::Ping(_))) | Some(Ok(WsMessage::Pong(_))) => continue,
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    Some(Err(e)) => { tracing::debug!("websocket error: {e}"); break }
                };
                match decoded {
                    Ok((channel, msg)) => {
                        if let Err(e) = client.send(channel, msg).await {
                            tracing::warn!("dropping client message: {e}");
                        }
                    }
                    Err(e) => tracing::warn!("undecodable websocket frame: {e}"),
                }
            }
            outgoing = client.recv() => {
                let Some((channel, msg)) = outgoing else {
                    // Kernel gone, or a newer connection took this
                    // session: tell the client why, as jupyter_server does.
                    let (code, reason) = if client.replaced() {
                        (1000, "replaced by a new connection for this session")
                    } else {
                        (1001, "kernel shut down")
                    };
                    let _ = socket.send(WsMessage::Close(Some(axum::extract::ws::CloseFrame {
                        code,
                        reason: reason.into(),
                    }))).await;
                    break;
                };
                let frame = if use_v1 {
                    v1::encode(channel, &msg).map(|b| WsMessage::Binary(b.into()))
                } else {
                    legacy::encode(channel, &msg).map(|f| match f {
                        legacy::Frame::Text(t) => WsMessage::Text(t.into()),
                        legacy::Frame::Binary(b) => WsMessage::Binary(b.into()),
                    })
                };
                match frame {
                    Ok(f) => {
                        if socket.send(f).await.is_err() { break }
                    }
                    Err(e) => tracing::warn!(%channel, "encode failed: {e}"),
                }
            }
        }
    }
    tracing::debug!(kernel = client.kernel_id(), "websocket closed");
}

/// `/api/events/subscribe`: `jupyter_events`' stream of server events.
/// JupyterLab keeps one open. One event per write the server makes of a
/// file is sent, in `jupyter_events`' envelope
/// ([`crate::write_observer::WriteEvent`]); nothing else is, so the socket
/// is quiet while the server writes nothing. A client that falls behind
/// is closed with 1011 `missed events`, and one that takes no frame for
/// ten seconds is dropped: its view of the files may be stale,
/// and forgetting what it can't confirm fails safe.
pub async fn events(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if !origin_allowed(&state, &headers) {
        return HttpError(403, "origin not allowed".into()).into_response();
    }
    // Taken before the upgrade, so a write between the request and it is
    // not missed.
    let writes = state.write_observer.subscribe();
    ws.on_upgrade(move |socket| run_events(socket, writes, EVENT_SEND_WAIT))
}

/// Sends `writes` to `socket` until either ends. A client that takes no
/// frame within `send_wait` has stopped reading and is dropped, with its
/// subscription. Over any socket, so a test can give it one that never
/// takes a frame.
async fn run_events<S, E>(
    mut socket: S,
    mut writes: broadcast::Receiver<WriteEvent>,
    send_wait: std::time::Duration,
) where
    S: futures::Stream<Item = Result<WsMessage, E>> + futures::Sink<WsMessage> + Unpin,
{
    use futures::{SinkExt, StreamExt};
    loop {
        tokio::select! {
            incoming = socket.next() => match incoming {
                Some(Ok(WsMessage::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            frame = next_event_frame(&mut writes) => {
                let Some(frame) = frame else { break };
                let closing = matches!(frame, WsMessage::Close(_));
                // A client that takes no data for this long has stopped
                // reading: drop it, and its subscription with it, rather
                // than wait on it while the channel moves on without it.
                match tokio::time::timeout(send_wait, socket.send(frame)).await {
                    Ok(Ok(())) if !closing => {}
                    _ => break,
                }
            }
        }
    }
}

/// How long the events socket waits for a client to take one frame before
/// it drops the client (#978).
const EVENT_SEND_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// What the events socket sends next: the next write as a text frame in
/// `jupyter_events`' envelope, or, when writes came faster than this
/// client read them, a close with 1011 `missed events` rather than events
/// with a gap it can't tell about. `None` once the channel is gone.
async fn next_event_frame(writes: &mut broadcast::Receiver<WriteEvent>) -> Option<WsMessage> {
    match writes.recv().await {
        Ok(write) => Some(WsMessage::Text(write.envelope().to_string().into())),
        Err(broadcast::error::RecvError::Lagged(_)) => {
            Some(WsMessage::Close(Some(axum::extract::ws::CloseFrame {
                code: 1011,
                reason: "missed events".into(),
            })))
        }
        Err(broadcast::error::RecvError::Closed) => None,
    }
}

/// `/terminals/websocket/{name}`: terminado's protocol, JSON arrays in text
/// frames. The server sends `["setup", {}]`, the terminal's kept output,
/// then `["stdout", text]` as it comes, and `["disconnect", 1]` when the
/// shell ends. The client sends `["stdin", text]` and
/// `["set_size", rows, cols, ...]`.
pub async fn terminal(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !origin_allowed(&state, &headers) {
        return HttpError(403, "origin not allowed".into()).into_response();
    }
    let client = match crate::routes::terminals(&state).and_then(|t| Ok(t.connect(&name)?)) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    ws.on_upgrade(move |socket| run_terminal(socket, client))
}

async fn run_terminal(mut socket: WebSocket, mut client: ganymede_terminals::TerminalClient) {
    use ganymede_terminals::Output;
    use serde_json::{json, Value};
    let text = |v: Value| WsMessage::Text(v.to_string().into());
    if socket.send(text(json!(["setup", {}]))).await.is_err() {
        return;
    }
    if let Some(h) = client.take_history() {
        if socket.send(text(json!(["stdout", h]))).await.is_err() {
            return;
        }
    }
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                let t = match incoming {
                    Some(Ok(WsMessage::Text(t))) => t,
                    Some(Ok(WsMessage::Binary(_) | WsMessage::Ping(_) | WsMessage::Pong(_))) => continue,
                    Some(Ok(WsMessage::Close(_))) | None | Some(Err(_)) => break,
                };
                let Ok(Value::Array(msg)) = serde_json::from_str::<Value>(t.as_str()) else {
                    tracing::debug!("undecodable terminal message");
                    continue;
                };
                match msg.first().and_then(Value::as_str) {
                    Some("stdin") => {
                        if let Some(data) = msg.get(1).and_then(Value::as_str) {
                            client.write(data);
                        }
                    }
                    Some("set_size") => {
                        let n = |i: usize| msg.get(i).and_then(Value::as_u64).and_then(|v| u16::try_from(v).ok());
                        if let (Some(rows), Some(cols)) = (n(1), n(2)) {
                            client.resize(rows, cols);
                        }
                    }
                    _ => {}
                }
            }
            out = client.recv() => match out {
                Some(Output::Stdout(s)) => {
                    if socket.send(text(json!(["stdout", s]))).await.is_err() { break }
                }
                Some(Output::Exited) | None => {
                    let _ = socket.send(text(json!(["disconnect", 1]))).await;
                    let _ = socket.send(WsMessage::Close(None)).await;
                    break;
                }
            }
        }
    }
    tracing::debug!(terminal = client.name(), "terminal websocket closed");
}

/// `/lsp/ws/{name}`: JSON-RPC with a language server, one message per
/// text frame and no `Content-Length` header, as `jupyter-lsp` relays it.
/// Every client of a server shares its process and sees all it sends.
pub async fn language_server(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !origin_allowed(&state, &headers) {
        return HttpError(403, "origin not allowed".into()).into_response();
    }
    let client = match crate::routes::lsp(&state).and_then(|m| Ok(m.connect(&name)?)) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    ws.on_upgrade(move |socket| run_language_server(socket, client))
}

async fn run_language_server(mut socket: WebSocket, mut client: ganymede_lsp::LspClient) {
    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(WsMessage::Text(t))) => client.send(t.as_str().to_owned()).await,
                Some(Ok(WsMessage::Binary(b))) => {
                    client.send(String::from_utf8_lossy(&b).into_owned()).await
                }
                Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_))) => {}
                Some(Ok(WsMessage::Close(_))) | None | Some(Err(_)) => break,
            },
            out = client.recv() => match out {
                Some(m) => {
                    if socket.send(WsMessage::Text(m.as_ref().into())).await.is_err() { break }
                }
                None => {
                    let _ = socket.send(WsMessage::Close(None)).await;
                    break;
                }
            }
        }
    }
    tracing::debug!(server = client.server(), "language server websocket closed");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::write_observer::WriteObserver;
    use std::path::Path;

    fn path_of(frame: Option<WsMessage>) -> String {
        let Some(WsMessage::Text(text)) = frame else {
            panic!("expected an event, got {frame:?}")
        };
        let event: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
        event["path"].as_str().unwrap().to_string()
    }

    /// A subscriber that falls behind is told so with 1011 `missed
    /// events`, while one that keeps up gets every write (#978).
    #[tokio::test]
    async fn a_lagging_subscriber_is_closed_with_1011_and_others_get_every_write() {
        let observer = WriteObserver::with_capacity(2);
        let mut behind = observer.subscribe();
        let mut reading = observer.subscribe();
        for i in 0..5 {
            let name = format!("{i}.ipynb");
            observer.notify(Path::new("/root/x"), &name, None, Some("after"), false);
            assert_eq!(path_of(next_event_frame(&mut reading).await), name);
        }
        match next_event_frame(&mut behind).await {
            Some(WsMessage::Close(Some(frame))) => {
                assert_eq!(frame.code, 1011);
                assert_eq!(frame.reason.as_str(), "missed events");
            }
            other => panic!("expected a close, got {other:?}"),
        }
    }

    /// A socket that never takes a frame and never sends one.
    struct Stuck;

    impl futures::Stream for Stuck {
        type Item = Result<WsMessage, ()>;
        fn poll_next(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::task::Poll::Pending
        }
    }

    impl futures::Sink<WsMessage> for Stuck {
        type Error = ();
        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), ()>> {
            std::task::Poll::Pending
        }
        fn start_send(self: std::pin::Pin<&mut Self>, _: WsMessage) -> Result<(), ()> {
            Ok(())
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), ()>> {
            std::task::Poll::Pending
        }
        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), ()>> {
            std::task::Poll::Pending
        }
    }

    /// A client that stops taking frames is dropped once a send has waited
    /// its time, and its subscription goes with it, so writes stop being
    /// stamped for it (#978).
    #[tokio::test]
    async fn a_client_that_takes_no_frame_is_dropped_with_its_subscription() {
        let observer = WriteObserver::new();
        let writes = observer.subscribe();
        let run = tokio::spawn(run_events(
            Stuck,
            writes,
            std::time::Duration::from_millis(50),
        ));
        observer.notify(Path::new("/root/x"), "a.ipynb", None, Some("after"), false);
        tokio::time::timeout(std::time::Duration::from_secs(10), run)
            .await
            .expect("the stuck client was not dropped")
            .unwrap();
        assert!(!observer.is_set(), "its subscription is gone");
    }

    /// The channel gone, there is nothing more to send.
    #[tokio::test]
    async fn no_frame_once_the_channel_is_gone() {
        let observer = WriteObserver::new();
        let mut writes = observer.subscribe();
        drop(observer);
        assert!(next_event_frame(&mut writes).await.is_none());
    }
}
