
use tokio;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

fn print_essential_info() {
    info!("Hello world");
    info!("- Version: {}", env!("CARGO_PKG_VERSION"));
    info!("- Description: {}", env!("CARGO_PKG_DESCRIPTION"));
    info!("My github : github.com/rosenthall");

    info!("Starting node");
}


#[tokio::main]
async fn main() {
    // a builder for `FmtSubscriber`.
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::TRACE)
        .finish();

    tracing::subscriber::set_global_default(subscriber)
        .expect("setting default subscriber failed");

    print_essential_info();
}
