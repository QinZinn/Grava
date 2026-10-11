//! Thin executable entry point; runtime and terminal ownership belong to `app`.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use clap::Parser;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        // `grava --man | head` closes the pipe early; that is not a failure.
        Err(error) if is_broken_pipe(&error) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("grava: {error}");
            ExitCode::FAILURE
        }
    }
}

fn is_broken_pipe(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<grava::app::AppError>(),
        Some(grava::app::AppError::Io(io)) if io.kind() == std::io::ErrorKind::BrokenPipe
    )
}

fn run() -> anyhow::Result<()> {
    grava::app::run(grava::cli::Cli::parse())?;
    Ok(())
}
