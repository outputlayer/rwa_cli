#[tokio::main]
async fn main() {
    if let Err(e) = rwa_cli::run().await {
        let is_json = std::env::args().any(|a| a == "--json");
        if is_json {
            let msg = e.to_string();
            let obj = serde_json::json!({
                "status": "error",
                "error": msg,
            });
            println!("{obj}");
        } else {
            eprintln!("Error: {e}");
        }
        std::process::exit(1);
    }
}
