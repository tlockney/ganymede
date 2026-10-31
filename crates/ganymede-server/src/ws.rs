//! `/api/kernels/{id}/channels`: one websocket per client, framed with the
//! v1 subprotocol when the client offers it and legacy JSON otherwise.

use crate::routes::{origin_allowed, HttpError};
use crate::AppState;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use ganymede_api::ClientConnection;
use ganymede_protocol::ws::{legacy, v1};

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
/// JupyterLab keeps one open. This server emits no events, so the socket
/// stays quiet until the client closes it.
pub async fn events(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if !origin_allowed(&state, &headers) {
        return HttpError(403, "origin not allowed".into()).into_response();
    }
    ws.on_upgrade(|mut socket| async move {
        while let Some(Ok(msg)) = socket.recv().await {
            if matches!(msg, WsMessage::Close(_)) {
                break;
            }
        }
    })
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
