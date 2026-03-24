#[tokio::main]
async fn main() {
    if let Err(e) = rwa_cli::run().await {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
