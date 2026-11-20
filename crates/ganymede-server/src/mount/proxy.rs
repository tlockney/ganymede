//! The router for `/machines/{id}/{*rest}`; see [`super`].

use crate::routes::HttpError;
use crate::routes::{FILES_CSP, PAGE_CSP};
use crate::AppState;
use axum::body::Body;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequest, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use futures::SinkExt;
use ganymede_gateway::mount::{route, Frame, Kind, Refused, Upstream};

pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .route("/machines/{id}/{*rest}", any(forward))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::mount,
        ))
        .with_state(state)
}

/// What follows `/machines/` in a request path, split at the id: the id and
/// the path after it, both as sent.
fn split(path: &str) -> Option<(&str, &str)> {
    path.strip_prefix("/machines/")?.split_once('/')
}

fn is_websocket(headers: &HeaderMap) -> bool {
    headers
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
}

async fn forward(State(state): State<AppState>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    let Some((id, rest)) = split(&path) else {
        return HttpError(404, "Not Found".into()).into_response();
    };
    let Some(machine) = state.machines.get(id) else {
        return HttpError(404, format!("No machine {id:?} is mounted")).into_response();
    };
    let kind = match route(rest) {
        Ok(k) => k,
        Err(Refused::Malformed) => return HttpError(400, "Bad path".into()).into_response(),
        Err(Refused::NotAllowed) => return HttpError(404, "Not Found".into()).into_response(),
    };
    let query = req.uri().query().map(String::from);
    // `base_url` ends in `/`.
    let prefix = format!("{}machines/{id}", state.base_url);
    if is_websocket(req.headers()) {
        return proxy_socket(machine, rest, query.as_deref(), req).await;
    }
    let (parts, body) = req.into_parts();
    if !matches!(
        parts.method,
        Method::GET | Method::HEAD | Method::PUT | Method::POST | Method::PATCH | Method::DELETE
    ) {
        return HttpError(405, "Method Not Allowed".into()).into_response();
    }
    let body = match axum::body::to_bytes(body, state.max_body_size).await {
        Ok(b) => b,
        Err(_) => return HttpError(413, "Request Entity Too Large".into()).into_response(),
    };
    let forwarded = match machine
        .forward(
            parts.method,
            rest,
            query.as_deref(),
            &parts.headers,
            body,
            &prefix,
        )
        .await
    {
        Ok(f) => f,
        Err(e) => return HttpError(502, e).into_response(),
    };
    let (status, mut headers) = (forwarded.status, forwarded.headers.clone());
    // A machine's HTML lands on this origin: whatever the route, run it in
    // an opaque one, as this server's own `/files` does, in place of any
    // policy the machine sent (websockets, which are not documents, return
    // above).
    let csp = match kind {
        Kind::Page => PAGE_CSP,
        Kind::Other => FILES_CSP,
    };
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(csp),
    );
    let mut resp = Response::new(Body::from_stream(forwarded.into_stream()));
    *resp.status_mut() = status;
    *resp.headers_mut() = headers;
    resp
}

/// The subprotocols a handshake offers, in the client's order.
fn offered(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// Opens the machine's socket first, so a machine that is down is a 502 and
/// not a socket that opens and closes, then accepts the page's with the
/// subprotocol the machine chose.
async fn proxy_socket(
    machine: std::sync::Arc<ganymede_gateway::mount::Machine>,
    rest: &str,
    query: Option<&str>,
    req: Request,
) -> Response {
    let protocols = offered(req.headers());
    let ws = match WebSocketUpgrade::from_request(req, &()).await {
        Ok(ws) => ws,
        Err(e) => return e.into_response(),
    };
    let upstream = match machine.connect(rest, query, &protocols).await {
        Ok(u) => u,
        Err(e) => return HttpError(502, e).into_response(),
    };
    let chosen: Vec<String> = upstream
        .protocol()
        .filter(|p| protocols.iter().any(|o| o == p))
        .map(String::from)
        .into_iter()
        .collect();
    ws.protocols(chosen)
        .on_upgrade(move |socket| pump(socket, upstream))
}

fn to_frame(msg: Message) -> Option<Frame> {
    match msg {
        Message::Text(t) => Some(Frame::Text(t.as_str().to_string())),
        Message::Binary(b) => Some(Frame::Binary(b.to_vec())),
        Message::Close(c) => Some(Frame::Close(
            c.map(|c| (c.code, c.reason.as_str().to_string())),
        )),
        Message::Ping(_) | Message::Pong(_) => None,
    }
}

fn from_frame(frame: Frame) -> Message {
    match frame {
        Frame::Text(t) => Message::Text(t.into()),
        Frame::Binary(b) => Message::Binary(b.into()),
        // Codes that must not go on the wire (1005, 1006) are left off.
        Frame::Close(c) => Message::Close(
            c.filter(|(code, _)| !matches!(code, 0..=999 | 1004..=1006 | 1015..=2999))
                .map(|(code, reason)| CloseFrame {
                    code,
                    reason: reason.into(),
                }),
        ),
    }
}

/// Frames both ways until either side ends or the machine is removed; then
/// both are closed.
async fn pump(mut socket: WebSocket, mut upstream: Upstream) {
    let mut removed = upstream.removal();
    let mut close_page: Option<Message> = None;
    let mut close_machine: Option<Frame> = None;
    loop {
        tokio::select! {
            msg = socket.recv() => {
                let Some(Ok(msg)) = msg else { break };
                let Some(frame) = to_frame(msg) else { continue };
                let closing = matches!(frame, Frame::Close(_));
                if upstream.send(frame).await.is_err() || closing {
                    break;
                }
            }
            frame = upstream.recv() => {
                let Some(frame) = frame else { break };
                let closing = matches!(frame, Frame::Close(_));
                if socket.send(from_frame(frame)).await.is_err() || closing {
                    break;
                }
            }
            () = removed.wait() => {
                close_page = Some(Message::Close(Some(CloseFrame {
                    code: 1001,
                    reason: "machine removed".into(),
                })));
                close_machine = Some(Frame::Close(Some((1001, "machine removed".into()))));
                break;
            }
        }
    }
    // Best effort: either side may be gone already.
    if let Some(m) = close_page {
        let _ = socket.send(m).await;
    }
    if let Some(f) = close_machine {
        let _ = upstream.send(f).await;
    }
    upstream.close().await;
    let _ = socket.close().await;
}
