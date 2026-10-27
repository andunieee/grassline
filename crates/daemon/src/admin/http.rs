//! The admin interface over HTTP with Server-Sent Events (the `hyperb`
//! protocol, `hyperb_core::http`). Connections from this machine are let in;
//! any others must prove a key from the allowed list.

use super::app::Admin;
use super::run::{run, Cmd};
use crate::core::{random_hex, Core};
use crate::hyperb::{line_to_event, PublicKey, KEY, SESSION, SIGNATURE};
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{self, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use futures_util::stream::{self, Stream, StreamExt};
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tower_http::cors::{Any, CorsLayer};

struct Session {
    key: PublicKey,
    cmds: mpsc::UnboundedSender<Cmd>,
    streaming: bool,
}

#[derive(Clone)]
struct St {
    core: Arc<Core>,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
}

pub fn router(core: Arc<Core>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([axum::http::Method::GET, axum::http::Method::POST])
        .allow_headers(
            [KEY, SESSION, SIGNATURE]
                .map(axum::http::HeaderName::from_static)
                .into_iter()
                .chain([header::CONTENT_TYPE])
                .collect::<Vec<_>>(),
        )
        .expose_headers([axum::http::HeaderName::from_static(SESSION)]);
    Router::new()
        .route("/", get(stream).post(post))
        .route("/{app}", get(stream).post(post))
        .layer(cors)
        .with_state(St {
            core,
            sessions: Default::default(),
        })
}

fn header_str<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name)?.to_str().ok()
}

fn allowed(core: &Core, peer: SocketAddr, key: &PublicKey) -> bool {
    peer.ip().is_loopback() || core.config().admin_keys.contains(&key.to_string())
}

async fn post(
    State(st): State<St>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if let Some(id) = header_str(&headers, SESSION) {
        let map = st.sessions.lock().unwrap();
        return match authorize(&map, id, &headers) {
            Ok(s) if s.cmds.send(Cmd::Input(body)).is_ok() => {
                StatusCode::NO_CONTENT.into_response()
            }
            Ok(_) => (StatusCode::GONE, "session ended").into_response(),
            Err(r) => r.into_response(),
        };
    }
    let Some(key) = header_str(&headers, KEY).and_then(|k| k.parse::<PublicKey>().ok()) else {
        return (StatusCode::BAD_REQUEST, format!("missing or invalid {KEY}")).into_response();
    };
    // The key is only claimed here; every later request proves it.
    if !allowed(&st.core, peer, &key) {
        return (
            StatusCode::FORBIDDEN,
            "this key is not allowed into the admin interface",
        )
            .into_response();
    }
    let id = random_hex(32);
    let (cmds, rx) = mpsc::unbounded_channel();
    st.sessions.lock().unwrap().insert(
        id.clone(),
        Session {
            key,
            cmds,
            streaming: false,
        },
    );
    let (sessions, sid) = (st.sessions.clone(), id.clone());
    tokio::spawn(async move {
        run::<Admin>(body, rx).await;
        sessions.lock().unwrap().remove(&sid);
    });
    ([(SESSION, id)], StatusCode::OK).into_response()
}

async fn stream(State(st): State<St>, headers: HeaderMap) -> Response {
    let wants_events =
        header_str(&headers, "accept").is_some_and(|a| a.contains("text/event-stream"));
    if !wants_events {
        let host = header_str(&headers, "host").unwrap_or("localhost");
        return Html(format!(
            "<!doctype html><meta charset=utf-8><title>grassline</title>\
             <p>This is the grassline admin interface. Open <code>hyperb://{host}/</code> in a \
             <a href=\"https://github.com/andunieee/hyperb\">hyperb</a> browser.</p>"
        ))
        .into_response();
    }
    let Some(id) = header_str(&headers, SESSION) else {
        return (StatusCode::UNAUTHORIZED, "no session").into_response();
    };
    let mut map = st.sessions.lock().unwrap();
    if let Err(r) = authorize(&map, id, &headers) {
        return r.into_response();
    }
    let s = map.get_mut(id).expect("authorized");
    if s.streaming {
        return (StatusCode::CONFLICT, "the session already has a stream").into_response();
    }
    let (tx, rx) = mpsc::unbounded_channel();
    if s.cmds.send(Cmd::Stream(tx)).is_err() {
        return (StatusCode::GONE, "session ended").into_response();
    }
    s.streaming = true;
    Sse::new(events(rx))
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn events(
    rx: mpsc::UnboundedReceiver<String>,
) -> impl Stream<Item = Result<sse::Event, Infallible>> {
    stream::unfold(rx, |mut rx| async move { Some((rx.recv().await?, rx)) }).flat_map(|chunk| {
        let events: Vec<_> = chunk
            .lines()
            .filter_map(line_to_event)
            .map(|(tag, json)| Ok(sse::Event::default().event(tag).data(json)))
            .collect();
        stream::iter(events)
    })
}

fn authorize<'a>(
    map: &'a HashMap<String, Session>,
    id: &str,
    headers: &HeaderMap,
) -> Result<&'a Session, (StatusCode, &'static str)> {
    let deny = |why| Err((StatusCode::UNAUTHORIZED, why));
    let Some(s) = map.get(id) else {
        return deny("unknown session");
    };
    let key = header_str(headers, KEY).and_then(|k| k.parse::<PublicKey>().ok());
    if key != Some(s.key) {
        return deny("the session belongs to another key");
    }
    if !s.key.verify(
        id.as_bytes(),
        header_str(headers, SIGNATURE).unwrap_or_default(),
    ) {
        return deny("bad signature");
    }
    Ok(s)
}
