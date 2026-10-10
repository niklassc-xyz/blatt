use std::{env, net::SocketAddr};

use blatt::{config::AppPaths, web};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "blatt=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let paths = AppPaths::discover()?;
    let address = env::var("BLATT_ADDR")
        .or_else(|_| env::var("RSS_READER_ADDR"))
        .unwrap_or_else(|_| "127.0.0.1:3000".to_owned())
        .parse::<SocketAddr>()?;

    web::serve(address, paths).await?;
    Ok(())
}
