//! The one binary: the playable game, or one scripted verification run.
//!
//! `midcreek-cs-1` opens the game. `midcreek-cs-1 --verify-output <directory>`
//! drives the same game through the deterministic verification journey and
//! writes its fourteen named frames and one canonical report there.
//!
//! An unusable output path is a usage error, not a crash: it exits with code 2
//! and one line on stderr naming the path and the reason.
//!
//! `midcreek-cs-1 --verify-flood <bytes>` is the pipe fixture: it writes that
//! many bytes to each of stdout and stderr and exits successfully, so the
//! parent watchdog's concurrent drain can be proven against a child that
//! really does overrun the platform pipe buffers.

use std::process::ExitCode;

#[cfg(target_arch = "wasm32")]
fn main() -> ExitCode {
    midcreek_cs_1::run();
    ExitCode::SUCCESS
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> ExitCode {
    use std::env;

    use midcreek_cs_1::verification::{VerifyOutput, parse_verification_args, run_flood};

    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let request = match parse_verification_args(arguments) {
        Ok(request) => request,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };

    if let Some(bytes) = request.flood {
        return run_flood(bytes);
    }

    let Some(path) = request.output else {
        midcreek_cs_1::run();
        return ExitCode::SUCCESS;
    };

    let output = match VerifyOutput::prepare(&path) {
        Ok(output) => output,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    if let Err(error) = output.clear() {
        eprintln!("could not clear stale verification artifacts: {error}");
        return ExitCode::from(2);
    }
    midcreek_cs_1::run_verification(output, request.fault)
}
