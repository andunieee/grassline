//! `grasslined`: a daemon that runs local coding agents headlessly, offers
//! them to remote controllers over iroh, and is administered through a
//! hypereffect interface (see `admin`) that starts on localhost only.

mod admin;
mod config;
mod control;
mod core;
mod hyperb;
mod iroh;
mod lines;
mod version;

use hyperb::Identity;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let config_dir = match (args.next().as_deref(), args.next()) {
        (Some("--version" | "-V"), _) => {
            println!("grasslined {}", version::full());
            return;
        }
        (Some("--help" | "-h"), _) => {
            println!(
                "grasslined {}\nusage: grasslined [--config-dir DIR]\nsettings and the iroh key live in DIR (default ~/.config/grassline); sessions in ~/.local/share/grassline",
                version::full()
            );
            return;
        }
        (Some("--config-dir"), Some(d)) => d.into(),
        _ => config::config_dir(),
    };
    println!("grasslined {}", version::full());
    let data_dir = std::env::var_os("GRASSLINE_DATA_DIR")
        .map(Into::into)
        .unwrap_or_else(config::data_dir);
    let core = core::Core::open(config_dir.join("config.json"), data_dir);
    let identity = match Identity::load_or_create(&config_dir.join("identity")) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("cannot load the identity: {e}");
            std::process::exit(1);
        }
    };
    admin::install(core.clone());
    tokio::spawn(iroh::serve(core.clone(), identity));
    tokio::spawn(admin::supervise_http(core.clone()));
    let _ = tokio::signal::ctrl_c().await;
    println!("bye");
}
