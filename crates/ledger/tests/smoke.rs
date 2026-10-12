use clap::Parser;
use ledger::cli::{Cli, run};

#[test]
fn subcommands_parse_and_run() {
    for sub in ["ingest", "report", "doctor"] {
        let cli = Cli::try_parse_from(["ledger", sub]).unwrap();
        let mut out = Vec::new();
        run(cli, &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains(sub));
    }
}
