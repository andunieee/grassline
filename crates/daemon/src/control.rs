//! The remote controller's session: the protocol of `grassline-proto` over
//! one iroh stream.

use crate::core::{Core, Update};
use grassline_proto::{parse, to_line, ClientMsg, ServerMsg};
use iroh::endpoint::{RecvStream, SendStream};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;

pub async fn serve(
    core: Arc<Core>,
    key: String,
    mut send: SendStream,
    mut recv: RecvStream,
) -> Result<(), String> {
    let mut lines = crate::lines::Lines::default();
    let first = lines
        .read_line(&mut recv)
        .await?
        .ok_or("closed before saying anything")?;
    let msg: ClientMsg = parse(&first).map_err(|e| e.to_string())?;

    let mut pending = None;
    let hello = if let ClientMsg::Pair { token, name } = &msg {
        if core.is_paired(&key) || core.pair(&key, name, token) {
            println!("paired {name} ({key})");
            ServerMsg::Hello {
                ok: true,
                error: None,
            }
        } else {
            ServerMsg::Hello {
                ok: false,
                error: Some("the pairing code is wrong or expired".into()),
            }
        }
    } else if core.is_paired(&key) {
        pending = Some(msg);
        ServerMsg::Hello {
            ok: true,
            error: None,
        }
    } else {
        ServerMsg::Hello {
            ok: false,
            error: Some("this device is not paired".into()),
        }
    };
    let ok = matches!(hello, ServerMsg::Hello { ok: true, .. });
    write(&mut send, &hello).await?;
    if !ok {
        let _ = send.finish();
        return Ok(());
    }

    let mut updates = core.subscribe();
    let mut changes = core.watch_config();
    let mut opened = HashSet::<String>::new();
    write(
        &mut send,
        &ServerMsg::Projects {
            items: core.projects(),
        },
    )
    .await?;
    write(&mut send, &agents(&core)).await?;
    write(
        &mut send,
        &ServerMsg::Sessions {
            items: core.sessions(),
        },
    )
    .await?;
    if let Some(m) = pending {
        handle(&core, &mut send, &mut opened, m).await?;
    }
    loop {
        tokio::select! {
            line = lines.read_line(&mut recv) => {
                let Some(line) = line? else { return Ok(()) };
                match parse::<ClientMsg>(&line) {
                    Ok(m) => handle(&core, &mut send, &mut opened, m).await?,
                    Err(e) => write(&mut send, &ServerMsg::Error { message: e.to_string() }).await?,
                }
            }
            u = updates.recv() => match u {
                Ok(Update::Session(info)) => write(&mut send, &ServerMsg::Session { info }).await?,
                Ok(Update::Removed(id)) => {
                    opened.remove(&id);
                    write(&mut send, &ServerMsg::Sessions { items: core.sessions() }).await?;
                }
                Ok(Update::Msg { id, index, message }) => {
                    if opened.contains(&id) {
                        write(&mut send, &ServerMsg::Msg { id, index, message }).await?;
                    }
                }
                // Missed some: start over.
                Err(RecvError::Lagged(_)) => {
                    write(&mut send, &ServerMsg::Sessions { items: core.sessions() }).await?;
                    for id in &opened {
                        let messages = core.messages(id).unwrap_or_default();
                        write(&mut send, &ServerMsg::History { id: id.clone(), messages }).await?;
                    }
                }
                Err(RecvError::Closed) => return Ok(()),
            },
            // A revoked device is let go.
            Ok(()) = changes.changed() => {
                if !core.is_paired(&key) {
                    let _ = send.finish();
                    return Ok(());
                }
                // The default agent or the agent binaries may have changed.
                write(&mut send, &agents(&core)).await?;
            }
        }
    }
}

async fn handle(
    core: &Arc<Core>,
    send: &mut SendStream,
    opened: &mut HashSet<String>,
    msg: ClientMsg,
) -> Result<(), String> {
    let err = |message: String| ServerMsg::Error { message };
    match msg {
        ClientMsg::Pair { .. } => {}
        ClientMsg::Refresh => {
            write(
                send,
                &ServerMsg::Projects {
                    items: core.projects(),
                },
            )
            .await?;
            write(send, &agents(core)).await?;
            write(
                send,
                &ServerMsg::Sessions {
                    items: core.sessions(),
                },
            )
            .await?;
        }
        // The settings change announces the new default.
        ClientMsg::SetDefaultAgent { provider } => {
            if let Err(e) = core.set_default_agent(&provider) {
                write(send, &err(e)).await?;
            }
        }
        ClientMsg::SetAgent { id, provider } => {
            if let Err(e) = core.set_agent(&id, &provider) {
                write(send, &err(e)).await?;
            }
        }
        ClientMsg::Create { project, provider } => {
            match core.create_session(&project, provider.as_deref()) {
                Ok(info) => {
                    opened.insert(info.id.clone());
                    // The `Session` update announces it; the history is empty.
                    write(
                        send,
                        &ServerMsg::History {
                            id: info.id,
                            messages: vec![],
                        },
                    )
                    .await?;
                }
                Err(e) => write(send, &err(e)).await?,
            }
        }
        ClientMsg::Open { id } => match core.messages(&id) {
            Some(messages) => {
                opened.insert(id.clone());
                write(send, &ServerMsg::History { id, messages }).await?;
            }
            None => write(send, &err("no such session".into())).await?,
        },
        ClientMsg::Send { id, text } => {
            if let Err(e) = core.send(&id, &text) {
                write(send, &err(e)).await?;
            }
        }
        ClientMsg::Stop { id } => core.stop(&id),
        ClientMsg::Decide {
            id,
            request,
            decision,
        } => core.decide(&id, &request, decision),
    }
    Ok(())
}

fn agents(core: &Core) -> ServerMsg {
    ServerMsg::Agents {
        items: core.agents(),
        default: core.config().default_provider,
    }
}

async fn write(send: &mut SendStream, m: &ServerMsg) -> Result<(), String> {
    send.write_all(to_line(m).as_bytes())
        .await
        .map_err(|e| e.to_string())
}
