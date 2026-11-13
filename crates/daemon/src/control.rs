//! The remote controller's session: the protocol of `grassline-proto` over
//! one iroh stream.

use crate::core::{Core, Update};
use crate::iroh::chain;
use grassline_proto::{encode, ClientMsg, Frames, Message, ServerMsg, MAX_VOICE_AUDIO};
use iroh::endpoint::{RecvStream, SendStream};
use std::collections::{HashMap, HashSet};
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
    let name = core.config().daemon_name();
    let mut announced = name.clone();
    let refuse = |why: &str| ServerMsg::Hello {
        ok: false,
        error: Some(why.into()),
        name: name.clone(),
    };
    let welcome = || ServerMsg::Hello {
        ok: true,
        error: None,
        name: name.clone(),
    };
    let hello = if let ClientMsg::Pair { token, name } = &msg {
        if core.is_paired(&key) || core.pair(&key, name, token) {
            println!("paired {name} ({key})");
            welcome()
        } else {
            refuse("the pairing code is wrong or expired")
        }
    } else if core.is_paired(&key) {
        pending = Some(msg);
        welcome()
    } else {
        refuse("this device is not paired")
    };
    let ok = matches!(hello, ServerMsg::Hello { ok: true, .. });
    write(&mut send, &hello).await?;
    if !ok {
        let _ = send.finish();
        return Ok(());
    }

    let mut updates = core.subscribe();
    let mut changes = core.watch_config();
    let mut opened = Opened::new(core.clone());
    // The voice messages that are still arriving, by request.
    let mut uploads = HashMap::<String, Vec<u8>>::new();
    // Answers that take a while, like the models.
    let (later, mut late) = mpsc::unbounded_channel::<ServerMsg>();
    write(&mut send, &projects(&core)).await?;
    write(&mut send, &agents(&core)).await?;
    write(&mut send, &transcription(&core)).await?;
    write(&mut send, &sessions(&core)).await?;
    if let Some(m) = pending {
        handle(&core, &mut send, &mut opened, &mut uploads, &later, m).await?;
    }
    loop {
        tokio::select! {
            m = read(&mut frames, &mut recv) => {
                let Some(m) = m? else { return Ok(()) };
                match m {
                    Ok(m) => handle(&core, &mut send, &mut opened, &mut uploads, &later, m).await?,
                    Err(message) => write(&mut send, &ServerMsg::Error { message }).await?,
                }
            }
            Some(m) = late.recv() => write(&mut send, &m).await?,
            u = updates.recv() => match u {
                Ok(Update::Session(info)) => write(&mut send, &ServerMsg::Session { info }).await?,
                Ok(Update::Projects) => write(&mut send, &projects(&core)).await?,
                Ok(Update::History(id)) => {
                    if opened.ids.contains(&id) {
                        let messages = core.messages(&id).unwrap_or_default();
                        write(&mut send, &ServerMsg::History { id, messages }).await?;
                    }
                }
                Ok(Update::Removed(id)) => {
                    opened.remove(&id);
                    write(&mut send, &ServerMsg::Removed { id }).await?;
                }
                Ok(Update::Msg { id, index, message }) => {
                    if opened.ids.contains(&id) {
                        write(&mut send, &ServerMsg::Msg { id, index, message }).await?;
                    }
                }
                // Missed some: start over.
                Err(RecvError::Lagged(_)) => {
                    write(&mut send, &sessions(&core)).await?;
                    for id in &opened.ids {
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
                // The project folders, the default agent, the agent
                // binaries or the transcription may have changed.
                write(&mut send, &projects(&core)).await?;
                write(&mut send, &agents(&core)).await?;
                write(&mut send, &transcription(&core)).await?;
                // So may have the name the app lists this machine under.
                let name = core.config().daemon_name();
                if name != announced {
                    announced = name.clone();
                    write(&mut send, &ServerMsg::Hello { ok: true, error: None, name }).await?;
                }
            }
        }
    }
}

async fn handle(
    core: &Arc<Core>,
    send: &mut SendStream,
    opened: &mut Opened,
    uploads: &mut HashMap<String, Vec<u8>>,
    later: &mpsc::UnboundedSender<ServerMsg>,
    msg: ClientMsg,
) -> Result<(), String> {
    let err = |message: String| ServerMsg::Error { message };
    match msg {
        ClientMsg::Pair { .. } => {}
        ClientMsg::Refresh => {
            write(send, &projects(core)).await?;
            write(send, &agents(core)).await?;
            write(send, &transcription(core)).await?;
            write(send, &sessions(core)).await?;
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
        ClientMsg::Resumable { id } => {
            let (core, later) = (core.clone(), later.clone());
            tokio::spawn(async move {
                let (items, error) = match core.resumable(&id).await {
                    Ok(items) => (items, None),
                    Err(e) => (vec![], Some(e)),
                };
                let _ = later.send(ServerMsg::Resumable { id, items, error });
            });
        }
        // The new history goes to whoever has the session open.
        ClientMsg::Resume { id, conversation } => {
            let (core, later) = (core.clone(), later.clone());
            tokio::spawn(async move {
                if let Err(message) = core.resume(&id, &conversation).await {
                    let _ = later.send(ServerMsg::Error { message });
                }
            });
        }
        ClientMsg::Transcribe {
            request,
            audio,
            last,
        } => {
            let held = uploads.get(&request).map_or(0, Vec::len);
            if audio.len() > MAX_VOICE_AUDIO.saturating_sub(held) {
                uploads.remove(&request);
                write(
                    send,
                    &ServerMsg::Transcribed {
                        request,
                        text: None,
                        error: Some("the voice message is too long".into()),
                    },
                )
                .await?;
            } else {
                uploads
                    .entry(request.clone())
                    .or_default()
                    .extend_from_slice(&audio);
                if last {
                    let audio = uploads.remove(&request).unwrap_or_default();
                    let (core, later) = (core.clone(), later.clone());
                    tokio::spawn(async move {
                        let msg = if audio.is_empty() {
                            ServerMsg::Transcribed {
                                request,
                                text: None,
                                error: Some("nothing was recorded".into()),
                            }
                        } else {
                            match core.transcribe_audio(audio).await {
                                Ok(text) => ServerMsg::Transcribed {
                                    request,
                                    text: Some(text),
                                    error: None,
                                },
                                Err(error) => ServerMsg::Transcribed {
                                    request,
                                    text: None,
                                    error: Some(error),
                                },
                            }
                        };
                        let _ = later.send(msg);
                    });
                }
            }
        }
        ClientMsg::Create { project, provider } => {
            match core.create_session(&project, provider.as_deref()) {
                Ok(info) => {
                    opened.insert(&info.id);
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
        ClientMsg::CreateProject {
            root,
            name,
            provider,
        } => {
            let created = core
                .create_project(&root, &name)
                .and_then(|path| core.create_session(&path, provider.as_deref()));
            match created {
                Ok(info) => {
                    opened.insert(&info.id);
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
        ClientMsg::Delete { id } => {
            opened.remove(&id);
            core.delete_session(&id);
        }
        ClientMsg::Open { id } => match opened.insert(&id) {
            Some(messages) => {
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

/// The sessions a connection has open, let go of when it ends.
struct Opened {
    core: Arc<Core>,
    ids: HashSet<String>,
}

impl Opened {
    fn new(core: Arc<Core>) -> Opened {
        Opened {
            core,
            ids: HashSet::new(),
        }
    }

    /// Opens session `id`, which marks it read; returns its messages.
    fn insert(&mut self, id: &str) -> Option<Vec<Message>> {
        if self.ids.contains(id) {
            return self.core.messages(id);
        }
        let messages = self.core.watch(id)?;
        self.ids.insert(id.into());
        Some(messages)
    }

    fn remove(&mut self, id: &str) {
        if self.ids.remove(id) {
            self.core.unwatch(id);
        }
    }
}

impl Drop for Opened {
    fn drop(&mut self) {
        for id in &self.ids {
            self.core.unwatch(id);
        }
    }
}

/// The sessions that are not archived.
fn sessions(core: &Core) -> ServerMsg {
    ServerMsg::Sessions {
        items: core
            .sessions()
            .into_iter()
            .filter(|s| !s.archived)
            .collect(),
    }
}

fn projects(core: &Core) -> ServerMsg {
    ServerMsg::Projects {
        items: core.projects(),
        roots: core.config().project_roots,
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
        config: core.config().transcription.resolve().map(Into::into),
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
        match recv.read(&mut buf).await.map_err(|e| chain(&e))? {
            Some(n) => frames.push(&buf[..n])?,
            None => return Ok(None),
        }
    }
}

async fn write(send: &mut SendStream, m: &ServerMsg) -> Result<(), String> {
    send.write_all(&encode(m)).await.map_err(|e| chain(&e))
}
