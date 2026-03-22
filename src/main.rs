#[tokio::main]
async fn main() {
    if let Err(error) = beam::run().await {
        eprintln!("beam: {error:#}");
        std::process::exit(1);
    }
}
