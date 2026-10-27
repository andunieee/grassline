//! Runs one hypereffect app instance for a transport: lines in, lines out.

use hypereffect::{App, Server};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

/// How long a session may wait for its event stream.
pub const STREAM_TIMEOUT: Duration = Duration::from_secs(30);

pub enum Cmd {
    /// Lines from the renderer.
    Input(String),
    /// The output stream opened; send everything there.
    Stream(mpsc::UnboundedSender<String>),
}

pub fn now_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64() * 1000.0
}

fn receive<A: App + Default>(server: &mut Server<A>, mut lines: String) {
    if !lines.ends_with('\n') {
        lines.push('\n');
    }
    server.receive(lines.as_bytes());
}

/// Runs an instance of `A` until its stream closes or the commands end.
pub async fn run<A: App + Default>(hello: String, mut cmds: mpsc::UnboundedReceiver<Cmd>) {
    let mut server = Server::<A>::new();
    receive(&mut server, hello);
    let mut backlog = String::new();
    let mut out: Option<mpsc::UnboundedSender<String>> = None;
    let mut ticker: Option<(u32, tokio::time::Interval)> = None;
    let deadline = tokio::time::sleep(STREAM_TIMEOUT);
    tokio::pin!(deadline);
    loop {
        backlog.push_str(&server.output().take());
        if let Some(tx) = &out {
            if !backlog.is_empty() && tx.send(std::mem::take(&mut backlog)).is_err() {
                break;
            }
        }
        let want = server.tick_interval();
        if want != ticker.as_ref().map(|t| t.0) {
            ticker = want.map(|ms| {
                let d = Duration::from_millis(ms.max(1).into());
                (ms, tokio::time::interval_at(tokio::time::Instant::now() + d, d))
            });
        }
        tokio::select! {
            cmd = cmds.recv() => match cmd {
                Some(Cmd::Input(lines)) => receive(&mut server, lines),
                Some(Cmd::Stream(tx)) => out = Some(tx),
                None => break,
            },
            _ = async { ticker.as_mut().unwrap().1.tick().await }, if ticker.is_some() => {
                server.tick(now_ms());
            }
            _ = &mut deadline, if out.is_none() => break,
            _ = async { out.as_ref().unwrap().closed().await }, if out.is_some() => break,
        }
    }
    server.close();
}
