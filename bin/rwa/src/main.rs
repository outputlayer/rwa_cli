use eyre::Result;

#[tokio::main]
async fn main() -> Result<()> {
    rwa_cli::run().await
}
