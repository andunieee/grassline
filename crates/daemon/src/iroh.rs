//! The daemon's iroh endpoint. One identity serves the remote controllers
//! (ALPN `grassline/0`) and, if the admin turned it on, the admin interface
//! to the allowed keys (ALPN `hyperb/0`).

use crate::core::Core;
use crate::{admin, control};
use crate::hyperb::{iroh_alpn_path, Identity, IROH_ALPN};
use iroh::endpoint::{presets, Connection, Incoming};
use iroh::{Endpoint, SecretKey};
use std::sync::Arc;

pub async fn serve(core: Arc<Core>, identity: Identity) {
    let alpns = vec![
        grassline_proto::ALPN.to_vec(),
        IROH_ALPN.as_bytes().to_vec(),
        format!("{IROH_ALPN}/admin").into_bytes(),
    ];
    let endpoint = match Endpoint::builder(presets::N0)
        .secret_key(SecretKey::from_bytes(&identity.secret()))
        .alpns(alpns)
        .bind()
        .await
    {
        Ok(e) => e,
        Err(e) => return eprintln!("no iroh: {e}"),
    };
    let _ = core.endpoint_id.set(endpoint.id().to_string());
    println!("iroh endpoint {}", endpoint.id());
    {
        let core = core.clone();
        let endpoint = endpoint.clone();
        tokio::spawn(async move {
            endpoint.online().await;
            crate::core::set_online(&core, true);
        });
    }
    while let Some(incoming) = endpoint.accept().await {
        let core = core.clone();
        tokio::spawn(async move {
            if let Err(e) = session(core, incoming).await {
                eprintln!("iroh session: {e}");
            }
        });
    }
}

async fn session(core: Arc<Core>, incoming: Incoming) -> Result<(), String> {
    let conn: Connection = incoming.accept().map_err(|e| e.to_string())?.await.map_err(|e| e.to_string())?;
    let key = conn.remote_id().to_string();
    let alpn = conn.alpn().to_vec();
    if alpn == grassline_proto::ALPN {
        let (send, recv) = conn.accept_bi().await.map_err(|e| e.to_string())?;
        let result = control::serve(core, key, send, recv).await;
        // Let the last bytes out before the connection goes away.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), conn.closed()).await;
        return result;
    }
    // The admin interface: only the keys the admin allowed.
    let path = iroh_alpn_path(&alpn).ok_or("unknown protocol")?;
    if !matches!(path.trim_matches('/'), "" | "admin") {
        return Err("no app here".into());
    }
    let cfg = core.config();
    if !cfg.admin_iroh || !cfg.admin_keys.contains(&key) {
        conn.close(1u32.into(), b"not allowed");
        return Err(format!("admin over iroh refused for {key}"));
    }
    let (send, recv) = conn.accept_bi().await.map_err(|e| e.to_string())?;
    println!("admin session for {key} over iroh");
    admin::serve_iroh(core, send, recv, conn).await
}
