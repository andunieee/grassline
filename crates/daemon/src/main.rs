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
mod local_speech;
mod version;

use clap::Parser;
use hyperb::Identity;
use std::path::PathBuf;

/// The daemon behind Grassline: runs coding agents on this machine and offers
/// them to remote controllers over iroh.
///
/// Settings and the iroh key live in the config directory (default
/// ~/.config/grassline); sessions in ~/.local/share/grassline.
#[derive(Parser)]
#[command(name = "grasslined", version = version::full())]
struct Args {
    /// Where settings and the iroh key live (default ~/.config/grassline)
    #[arg(long, value_name = "DIR")]
    config_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let config_dir = args.config_dir.unwrap_or_else(config::config_dir);
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
