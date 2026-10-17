//! `/api/kernels/{id}/channels`: one websocket per client, framed with the
//! v1 subprotocol when the client offers it and legacy JSON otherwise.

use crate::routes::{origin_allowed, HttpError};
use crate::AppState;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use ganymede_api::ClientConnection;
use ganymede_protocol::ws::{legacy, v1};

pub async fn channels(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !origin_allowed(&state, &headers) {
        return HttpError(403, "origin not allowed".into()).into_response();
    }
    let client = match state.kernels.connect_client(&id) {
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
                    // Kernel gone: tell the client why, as jupyter_server does.
                    let _ = socket.send(WsMessage::Close(Some(axum::extract::ws::CloseFrame {
                        code: 1001,
                        reason: "kernel shut down".into(),
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
