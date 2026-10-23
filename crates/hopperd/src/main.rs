//! Hopper Server (`hopperd`): a standalone server panel for Minecraft
//! servers, with a web UI and the API Hopper desktop drives. Plan:
//! design/server-panel.md. This is the empty start (M0): it only proves the
//! crate builds against hopper-core.

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("hopperd {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let servers = hopper_core::hosting::LocalPanel::new();
    let _ = &servers;
    log::info!("hopperd {} (nothing to serve yet)", env!("CARGO_PKG_VERSION"));
    Ok(())
}
