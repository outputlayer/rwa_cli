#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(e) = rwa_cli::run().await {
        let is_json = std::env::args().any(|a| a == "--json");
        rwa_cli::render_error(&e, is_json);
        std::process::exit(1);
    }
}
