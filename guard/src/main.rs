//! The upload guard's command line: `serve`, `check` and the bounded `worker` (see the library's documentation).
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::ExitCode;

fn run(args: &[String]) -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_current_span(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    // Large stacks: request futures nest deeply and the rule engine runs on the calling thread.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(8 << 20)
        .build()?;
    match args.get(1).map(String::as_str) {
        Some("serve") => {
            let config = args
                .iter()
                .position(|a| a == "--config")
                .and_then(|i| args.get(i + 1))
                .context("--config is required")?;
            runtime.block_on(enrichedlab_guard::serve(PathBuf::from(config)))
        }
        Some("check") => runtime.block_on(enrichedlab_guard::check(&args[2..])),
        _ => bail!(
            "usage: enrichedlab-guard serve --config FILE | check --policy FILE --use-case NAME --rules DIR [--scanner HOST:PORT] [--declared TYPE] [--write PATH] FILE"
        ),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    // The worker reports on standard output and logs nothing; the service reads its exit code and first error line.
    if args.get(1).map(String::as_str) == Some("worker") {
        return enrichedlab_guard::worker::main(&args[2..]);
    }
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
