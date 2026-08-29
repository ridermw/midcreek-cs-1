use std::{
    env,
    path::{Path, PathBuf},
};

use midcreek_cs_1::assetgen::{
    ASSET_NAMES, AssetGenError, GENERATED_DIR, check_assets, write_assets,
};

fn main() {
    match run() {
        Ok(summary) => println!("{summary}"),
        Err(message) => {
            eprintln!("assetgen: {message}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<String, String> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let mut mode = None;
    let mut root = None;
    let mut output = None;

    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--write" => mode = Some(Mode::Write),
            "--check" => mode = Some(Mode::Check),
            "--root" => {
                index += 1;
                root = Some(PathBuf::from(value(&arguments, index, "--root")?));
            }
            "--out" => {
                index += 1;
                output = Some(PathBuf::from(value(&arguments, index, "--out")?));
            }
            other => return Err(format!("unrecognized argument {other:?}\n{USAGE}")),
        }
        index += 1;
    }

    let mode =
        mode.ok_or_else(|| format!("exactly one of --write or --check is required\n{USAGE}"))?;
    let root = match root {
        Some(root) => root,
        None => env::current_dir()
            .map_err(|error| format!("cannot read the working directory: {error}"))?,
    };
    let sources = root.join(midcreek_cs_1::assetgen::SOURCE_DIR);
    if !sources.is_dir() {
        return Err(format!(
            "{} does not contain {}",
            root.display(),
            midcreek_cs_1::assetgen::SOURCE_DIR
        ));
    }

    match mode {
        Mode::Write => {
            let target = output.unwrap_or_else(|| root.join(GENERATED_DIR));
            let written = write_assets(&root, &target).map_err(describe)?;
            Ok(format!(
                "wrote {} assets to {}:\n{}",
                written.len(),
                target.display(),
                list(&written)
            ))
        }
        Mode::Check => {
            let report = check_assets(&root).map_err(describe)?;
            Ok(format!(
                "checked {} of {} assets against a double generation: {}",
                report.checked.len(),
                ASSET_NAMES.len(),
                report.checked.join(", ")
            ))
        }
    }
}

fn value(arguments: &[String], index: usize, flag: &str) -> Result<String, String> {
    arguments
        .get(index)
        .cloned()
        .ok_or_else(|| format!("{flag} requires a directory\n{USAGE}"))
}

fn list(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| format!("  {}", display(path)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn display(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn describe(error: AssetGenError) -> String {
    error.to_string()
}

enum Mode {
    Write,
    Check,
}

const USAGE: &str = "usage: assetgen (--write | --check) [--root <directory>] [--out <directory>]";
