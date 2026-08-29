use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, ExitCode},
};

use midcreek_cs_1::sitegen::{
    ProgressDocument, ProgressStatus, ReferenceManifest, RepoFacts, SiteInputs,
    VerificationSummary, WorkflowSummary, assemble_site, build_site, plan_task_ids_from_markdown,
    validate_progress, validate_reference_manifest,
};
use serde::Deserialize;

enum Command {
    Validate {
        progress: PathBuf,
        plan: PathBuf,
        repository: PathBuf,
    },
    Build {
        inputs: PathBuf,
        output: PathBuf,
    },
    Assemble {
        previous: Option<PathBuf>,
        current: PathBuf,
        result: PathBuf,
        output: PathBuf,
    },
}

fn main() -> ExitCode {
    let command = match parse_command(env::args().skip(1)) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("usage: sitegen <validate|build|assemble> [options]");
            return ExitCode::from(2);
        }
    };

    match command {
        Command::Validate {
            progress,
            plan,
            repository,
        } => validate(&progress, &plan, &repository),
        Command::Build { inputs, output } => build(&inputs, &output),
        Command::Assemble {
            previous,
            current,
            result,
            output,
        } => assemble(previous.as_deref(), &current, &result, &output),
    }
}

fn parse_command(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut args = args.into_iter();
    let name = args.next().ok_or_else(|| "missing command".to_owned())?;
    let remaining = args.collect::<Vec<_>>();

    match name.as_str() {
        "validate" => {
            let values = parse_options(&remaining, &["--progress", "--plan", "--repository"], &[])?;
            Ok(Command::Validate {
                progress: values.required("--progress")?,
                plan: values.required("--plan")?,
                repository: values.required("--repository")?,
            })
        }
        "build" => {
            let values = parse_options(&remaining, &["--inputs", "--output"], &[])?;
            Ok(Command::Build {
                inputs: values.required("--inputs")?,
                output: values.required("--output")?,
            })
        }
        "assemble" => {
            let values = parse_options(
                &remaining,
                &["--current", "--result", "--output"],
                &["--previous"],
            )?;
            Ok(Command::Assemble {
                previous: values.optional("--previous"),
                current: values.required("--current")?,
                result: values.required("--result")?,
                output: values.required("--output")?,
            })
        }
        _ => Err(format!("unknown command: {name}")),
    }
}

struct ParsedOptions {
    values: Vec<(String, PathBuf)>,
}

impl ParsedOptions {
    fn required(&self, name: &str) -> Result<PathBuf, String> {
        self.optional(name)
            .ok_or_else(|| format!("missing required option {name}"))
    }

    fn optional(&self, name: &str) -> Option<PathBuf> {
        self.values
            .iter()
            .find(|(option, _)| option == name)
            .map(|(_, value)| value.clone())
    }
}

fn parse_options(
    args: &[String],
    required: &[&str],
    optional: &[&str],
) -> Result<ParsedOptions, String> {
    let (pairs, remainder) = args.as_chunks::<2>();
    if !remainder.is_empty() {
        return Err("every option requires a value".to_owned());
    }

    let allowed = required
        .iter()
        .chain(optional)
        .copied()
        .collect::<BTreeSet<_>>();
    let mut values = Vec::new();

    for pair in pairs {
        let option = pair[0].as_str();
        if !allowed.contains(option) {
            return Err(format!("unknown option: {option}"));
        }
        if values
            .iter()
            .any(|(existing, _): &(String, PathBuf)| existing == option)
        {
            return Err(format!("duplicate option: {option}"));
        }
        values.push((option.to_owned(), PathBuf::from(&pair[1])));
    }

    let parsed = ParsedOptions { values };
    for option in required {
        if parsed.optional(option).is_none() {
            return Err(format!("missing required option {option}"));
        }
    }
    Ok(parsed)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SiteInputPaths {
    progress: PathBuf,
    plan: PathBuf,
    reference_manifest: PathBuf,
    workflow: PathBuf,
    repo: PathBuf,
    verification: Option<PathBuf>,
}

fn build(inputs_path: &Path, output: &Path) -> ExitCode {
    let paths = match read_json::<SiteInputPaths>(inputs_path) {
        Ok(paths) => paths,
        Err(message) => return content_error(message),
    };
    let root = inputs_path.parent().unwrap_or_else(|| Path::new("."));
    let progress_path = root.join(paths.progress);
    let plan_path = root.join(paths.plan);
    let reference_path = root.join(paths.reference_manifest);
    let workflow_path = root.join(paths.workflow);
    let repo_path = root.join(paths.repo);
    let progress = match read_json::<ProgressDocument>(&progress_path) {
        Ok(value) => value,
        Err(message) => return content_error(message),
    };
    let plan_markdown = match fs::read_to_string(&plan_path) {
        Ok(value) => value,
        Err(error) => return content_error(format!("{}: {error}", plan_path.display())),
    };
    let reference_manifest = match read_json::<ReferenceManifest>(&reference_path) {
        Ok(value) => value,
        Err(message) => return content_error(message),
    };
    let workflow = match read_json::<WorkflowSummary>(&workflow_path) {
        Ok(value) => value,
        Err(message) => return content_error(message),
    };
    let repo = match read_json::<RepoFacts>(&repo_path) {
        Ok(value) => value,
        Err(message) => return content_error(message),
    };
    let verification = match paths.verification {
        Some(path) => match read_json::<VerificationSummary>(&root.join(path)) {
            Ok(value) => Some(value),
            Err(message) => return content_error(message),
        },
        None => None,
    };
    let inputs = SiteInputs {
        progress,
        plan_markdown,
        reference_manifest,
        verification,
        workflow,
        repo,
    };

    match build_site(&inputs, output) {
        Ok(manifest) => {
            println!("{}", manifest.source_commit);
            ExitCode::SUCCESS
        }
        Err(error) => content_error(error.to_string()),
    }
}

fn assemble(
    previous: Option<&Path>,
    current: &Path,
    result_path: &Path,
    output: &Path,
) -> ExitCode {
    let workflow = match read_json::<WorkflowSummary>(result_path) {
        Ok(value) => value,
        Err(message) => return content_error(message),
    };
    match assemble_site(previous, current, &workflow, output) {
        Ok(disposition) => {
            println!("{disposition:?}");
            ExitCode::SUCCESS
        }
        Err(error) => content_error(error.to_string()),
    }
}

fn validate(progress_path: &Path, plan_path: &Path, repository: &Path) -> ExitCode {
    let progress = match read_progress(progress_path) {
        Ok(progress) => progress,
        Err(message) => return content_error(message),
    };
    let plan = match fs::read_to_string(plan_path) {
        Ok(plan) => plan,
        Err(error) => {
            return content_error(format!("{}: {error}", plan_path.display()));
        }
    };
    let repo = match read_repo_facts(repository) {
        Ok(repo) => repo,
        Err(message) => return content_error(message),
    };
    let reference_manifest_path = repository.join("docs/reference/manifest.json");
    let reference_manifest = match read_json::<ReferenceManifest>(&reference_manifest_path) {
        Ok(manifest) => manifest,
        Err(message) => return content_error(message),
    };

    if let Err(errors) = validate_progress(&progress, &plan_task_ids_from_markdown(&plan), &repo) {
        for error in errors {
            eprintln!("{error}");
        }
        return ExitCode::from(1);
    }
    if let Err(errors) = validate_reference_manifest(&reference_manifest, repository) {
        for error in errors {
            eprintln!("{error}");
        }
        return ExitCode::from(1);
    }

    let current = progress
        .tasks
        .iter()
        .find(|task| task.status == ProgressStatus::InProgress)
        .map_or("all-done", |task| task.id.as_str());
    println!("{current}");
    ExitCode::SUCCESS
}

fn read_progress(path: &Path) -> Result<ProgressDocument, String> {
    read_json(path)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let json = fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    serde_json::from_str(&json).map_err(|error| format!("{}: {error}", path.display()))
}

fn read_repo_facts(repository: &Path) -> Result<RepoFacts, String> {
    let head_sha = git_output(repository, &["rev-parse", "HEAD"])?;
    let known_commits = git_output(repository, &["rev-list", "--all"])?
        .lines()
        .map(str::to_owned)
        .collect();
    Ok(RepoFacts {
        head_sha,
        known_commits,
        commits: Vec::new(),
    })
}

fn git_output(repository: &Path, args: &[&str]) -> Result<String, String> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .output()
        .map_err(|error| format!("could not run git in {}: {error}", repository.display()))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            repository.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn content_error(message: String) -> ExitCode {
    eprintln!("{message}");
    ExitCode::from(1)
}
