use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "procly", version, about)]
struct Cli {}

fn main() {
    Cli::parse();
}
