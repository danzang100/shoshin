#[tokio::main]
async fn main() -> std::process::ExitCode {
    lightning::cli::main_entry().await
}
