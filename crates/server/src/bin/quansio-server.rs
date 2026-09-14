//! The deployable: one process running the composed server (APP-001).

use quansio_server::composition::{Composition, ServerConfig};

#[tokio::main]
async fn main() {
    let config = match ServerConfig::from_env() {
        Ok(config) => config,
        Err(message) => {
            eprintln!("quansio-server: {message}");
            std::process::exit(2);
        }
    };
    let bind = config.bind.clone();
    match Composition::new(config).await {
        Ok(composition) => {
            eprintln!("quansio-server: listening on {bind}");
            if let Err(message) = composition.serve().await {
                eprintln!("quansio-server: {message}");
                std::process::exit(1);
            }
        }
        Err(message) => {
            eprintln!("quansio-server: {message}");
            std::process::exit(1);
        }
    }
}
