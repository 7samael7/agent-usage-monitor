//! Repository chores that need real code rather than a shell one-liner.
//!
//! `cargo xtask <task>`. Today there is one, and it is the one that stops an
//! unredacted transcript reaching a remote.

mod redact;

use std::process::ExitCode;

const USAGE: &str = "\
cargo xtask <task>

  redact <input.jsonl> <output.jsonl> [--max-lines N]
      Turn a real agent transcript into a committable fixture: every number
      and structural field kept, every piece of free text replaced.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> anyhow::Result<()> {
    match args.split_first() {
        None => {
            print!("{USAGE}");
            Ok(())
        }
        Some((task, rest)) => match task.as_str() {
            "redact" => redact::main(rest),
            "-h" | "--help" | "help" => {
                print!("{USAGE}");
                Ok(())
            }
            other => anyhow::bail!("unknown task `{other}`\n\n{USAGE}"),
        },
    }
}
