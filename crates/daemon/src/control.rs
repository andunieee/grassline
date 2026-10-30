//! The remote controller's session: the protocol of `grassline-proto` over
//! one iroh stream.

use crate::core::{Core, Update};
use grassline_proto::{encode, ClientMsg, Frames, ServerMsg};
use iroh::endpoint::{RecvStream, SendStream};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;

pub async fn serve(
    core: Arc<Core>,
    key: String,
    mut send: SendStream,
    mut recv: RecvStream,
) -> Result<(), String> {
    let mut frames = Frames::default();
    let msg = read(&mut frames, &mut recv)
        .await?
        .ok_or("closed before saying anything")??;

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
    // Answers that take a while, like the models.
    let (later, mut late) = mpsc::unbounded_channel::<ServerMsg>();
    write(
        &mut send,
        &ServerMsg::Projects {
            items: core.projects(),
        },
    )
    .await?;
    write(&mut send, &agents(&core)).await?;
    write(&mut send, &transcription(&core)).await?;
    write(
        &mut send,
        &sessions(&core),
    )
    .await?;
    if let Some(m) = pending {
        handle(&core, &mut send, &mut opened, &later, m).await?;
    }
    loop {
        tokio::select! {
            m = read(&mut frames, &mut recv) => {
                let Some(m) = m? else { return Ok(()) };
                match m {
                    Ok(m) => handle(&core, &mut send, &mut opened, &later, m).await?,
                    Err(message) => write(&mut send, &ServerMsg::Error { message }).await?,
                }
            }
            Some(m) = late.recv() => write(&mut send, &m).await?,
            u = updates.recv() => match u {
                Ok(Update::Session(info)) => write(&mut send, &ServerMsg::Session { info }).await?,
                Ok(Update::Removed(id)) => {
                    opened.remove(&id);
                    write(&mut send, &sessions(&core)).await?;
                }
                Ok(Update::Msg { id, index, message }) => {
                    if opened.contains(&id) {
                        write(&mut send, &ServerMsg::Msg { id, index, message }).await?;
                    }
                }
                // Missed some: start over.
                Err(RecvError::Lagged(_)) => {
                    write(&mut send, &sessions(&core)).await?;
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
                // The default agent, the agent binaries or the
                // transcription may have changed.
                write(&mut send, &agents(&core)).await?;
                write(&mut send, &transcription(&core)).await?;
            }
        }
    }
}

async fn handle(
    core: &Arc<Core>,
    send: &mut SendStream,
    opened: &mut HashSet<String>,
    later: &mpsc::UnboundedSender<ServerMsg>,
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
            write(send, &transcription(core)).await?;
            write(
                send,
                &sessions(core),
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
        ClientMsg::SetModel { id, model } => {
            if let Err(e) = core.set_model(&id, &model) {
                write(send, &err(e)).await?;
            }
        }
        ClientMsg::SetPermissions { id, permissions } => {
            if let Err(e) = core.set_permissions(&id, permissions) {
                write(send, &err(e)).await?;
            }
        }
        ClientMsg::Models { provider } => {
            let (core, later) = (core.clone(), later.clone());
            tokio::spawn(async move {
                let (items, error) = match core.models(&provider).await {
                    Ok(items) => (items, None),
                    Err(e) => (vec![], Some(e)),
                };
                let _ = later.send(ServerMsg::Models {
                    provider,
                    items,
                    error,
                });
            });
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
        ClientMsg::Archive { id, archived } => {
            if let Err(e) = core.set_archived(&id, archived) {
                write(send, &err(e)).await?;
            }
        }
        ClientMsg::Archived => {
            let items = core.sessions().into_iter().filter(|s| s.archived).collect();
            write(send, &ServerMsg::Archived { items }).await?;
        }
        ClientMsg::Open { id } => match core.messages(&id) {
            Some(messages) => {
                opened.insert(id.clone());
                write(send, &ServerMsg::History { id, messages }).await?;
            }
            None => write(send, &err("no such session".into())).await?,
        },
        ClientMsg::Close { id } => {
            opened.remove(&id);
            core.delete_if_unused(&id);
        }
        ClientMsg::Send { id, text } => {
            if let Err(e) = core.send(&id, &text) {
                write(send, &err(e)).await?;
            }
        }
        ClientMsg::SendNow { id, queued } => {
            if let Err(e) = core.send_now(&id, &queued) {
                write(send, &err(e)).await?;
            }
        }
        ClientMsg::Unqueue { id, queued } => {
            if let Err(e) = core.unqueue(&id, &queued) {
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

/// The sessions that are not archived.
fn sessions(core: &Core) -> ServerMsg {
    ServerMsg::Sessions {
        items: core.sessions().into_iter().filter(|s| !s.archived).collect(),
    }
}

fn agents(core: &Core) -> ServerMsg {
    ServerMsg::Agents {
        items: core.agents(),
        default: core.config().default_provider,
    }
}

fn transcription(core: &Core) -> ServerMsg {
    ServerMsg::Transcription {
        config: core.config().transcription.resolve(),
    }
}

/// The next message, or `None` at the end of the stream; the inner error is
/// a message that does not decode. Cancel-safe.
async fn read(
    frames: &mut Frames,
    recv: &mut RecvStream,
) -> Result<Option<Result<ClientMsg, String>>, String> {
    let mut buf = [0u8; 16 * 1024];
    loop {
        if let Some(m) = frames.next() {
            return Ok(Some(m));
        }
        match recv.read(&mut buf).await.map_err(|e| e.to_string())? {
            Some(n) => frames.push(&buf[..n])?,
            None => return Ok(None),
        }
    }
}

async fn write(send: &mut SendStream, m: &ServerMsg) -> Result<(), String> {
    send.write_all(&encode(m)).await.map_err(|e| e.to_string())
}
