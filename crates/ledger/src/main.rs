use clap::Parser;

fn main() {
    let cli = ledger::cli::Cli::parse();
    let mut out = std::io::stdout().lock();
    if let Err(e) = ledger::cli::run(cli, &mut out) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
