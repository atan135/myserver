mod config;
#[allow(dead_code)]
mod pb {
    include!(concat!(env!("OUT_DIR"), "/myserver.game.rs"));
}
mod protocol;
mod server;
mod session;
mod ticket;

use config::Config;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(config.log_level.clone()))
        .with_target(false)
        .compact()
        .init();

    server::run(&config).await
}
