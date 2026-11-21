//! The admin interface: a hypereffect app, served over HTTP and, if enabled,
//! over iroh. The listener follows the settings that the app itself edits.

mod app;
mod http;
mod qr;
mod run;

use crate::core::Core;
use crate::lines::Lines;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use run::{run, Cmd};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

pub use app::install;

/// What the HTTP listener is doing, for the interface to show.
pub static HTTP_STATUS: Mutex<String> = Mutex::new(String::new());

fn set_status(s: String) {
    *HTTP_STATUS.lock().unwrap() = s;
}

/// Binds `addr`; if `walk`, moves to the next ports while the one asked for is taken.
async fn bind_free(addr: &str, walk: bool) -> std::io::Result<tokio::net::TcpListener> {
    let Ok(mut sock) = addr.parse::<std::net::SocketAddr>() else {
        return tokio::net::TcpListener::bind(addr).await;
    };
    loop {
        match tokio::net::TcpListener::bind(sock).await {
            Err(e) if walk && e.kind() == std::io::ErrorKind::AddrInUse && sock.port() < u16::MAX => {
                sock.set_port(sock.port() + 1);
            }
            r => return r,
        }
    }
}

/// Keeps the HTTP listener matching the settings. With `port`, that exact port on the saved
/// host is served instead, without looking for another one, and the saved address is ignored.
pub async fn supervise_http(core: Arc<Core>, port: Option<u16>) {
    let mut changes = core.watch_config();
    let mut current: Option<(String, tokio::task::JoinHandle<()>)> = None;
    loop {
        let mut want = core.config().admin_http;
        if let Some(port) = port {
            want.addr = match want.addr.parse::<std::net::SocketAddr>() {
                Ok(mut a) => {
                    a.set_port(port);
                    a.to_string()
                }
                Err(_) => format!("127.0.0.1:{port}"),
            };
        }
        let have = current.as_ref().map(|c| c.0.as_str());
        match (want.enabled, have) {
            (false, Some(_)) => {
                current.take().unwrap().1.abort();
                set_status("off".into());
            }
            (false, None) => set_status("off".into()),
            (true, h) if h == Some(want.addr.as_str()) => {}
            (true, _) => match bind_free(&want.addr, port.is_none()).await {
                Ok(listener) => {
                    let bound = listener
                        .local_addr()
                        .map_or_else(|_| want.addr.clone(), |a| a.to_string());
                    if let Some((_, old)) = current.take() {
                        old.abort();
                    }
                    let app = http::router(core.clone())
                        .into_make_service_with_connect_info::<std::net::SocketAddr>();
                    let task = tokio::spawn(async move {
                        if let Err(e) = axum::serve(listener, app).await {
                            eprintln!("admin http: {e}");
                        }
                    });
                    println!("admin interface on hyperb://{bound}/");
                    set_status(match bound == want.addr {
                        true => format!("listening on {bound}"),
                        false => format!("listening on {bound} ({} is in use)", want.addr),
                    });
                    current = Some((want.addr.clone(), task));
                }
                Err(e) => {
                    eprintln!("cannot listen on {}: {e}", want.addr);
                    set_status(match current {
                        Some(ref c) => {
                            format!("could not bind {} ({e}); still on {}", want.addr, c.0)
                        }
                        None => format!("could not bind {} ({e})", want.addr),
                    });
                }
            },
        }
        if changes.changed().await.is_err() {
            return;
        }
    }
}

/// An admin session over an iroh stream.
pub async fn serve_iroh(
    _core: Arc<Core>,
    mut send: SendStream,
    mut recv: RecvStream,
    conn: Connection,
) -> Result<(), String> {
    let mut lines = Lines::default();
    // QUIC announces the stream when the hello arrives.
    let hello = lines
        .read_line(&mut recv)
        .await?
        .ok_or("closed before hello")?;
    let (cmds, rx) = mpsc::unbounded_channel();
    let (out, mut out_rx) = mpsc::unbounded_channel();
    let _ = cmds.send(Cmd::Stream(out));
    let app = tokio::spawn(run::<app::Admin>(hello, rx));
    let read = async {
        while let Some(line) = lines.read_line(&mut recv).await? {
            if cmds.send(Cmd::Input(line)).is_err() {
                break;
            }
        }
        Ok::<_, String>(())
    };
    let write = async {
        while let Some(chunk) = out_rx.recv().await {
            send.write_all(chunk.as_bytes())
                .await
                .map_err(|e| e.to_string())?;
        }
        let _ = send.finish();
        let _ = conn.closed().await;
        Ok::<_, String>(())
    };
    let result = tokio::select! { r = read => r, r = write => r };
    drop(cmds);
    let _ = app.await;
    result
}
