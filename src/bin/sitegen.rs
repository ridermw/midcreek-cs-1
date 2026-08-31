//! The repository-owned site generator.
//!
//! Publication reads the repository, decodes verified frames, and writes
//! files, so the generator is a native tool. The browser target compiles this
//! binary to an inert entry point rather than carrying any of it.

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::{
        collections::BTreeSet,
        env, fs,
        path::{Path, PathBuf},
        process::{Command as ProcessCommand, ExitCode},
    };

    use midcreek_cs_1::{
        sitegen::{
            BrowserGateReport, CommitSummary, CurrentPublication, GalleryManifest, GateStatus,
            GateSummary, JobOutcome, JobReport, JobResult, PlayableBuild, ProgressDocument,
            ProgressStatus, RESULT_FILE, ReferenceManifest, RepoFacts, SiteInputs,
            VerificationEvidence, WorkflowSummary, assemble_site, build_site, gate_verdict,
            merge_job_results, missing_playable_parts, plan_task_ids_from_markdown,
            read_gate_records, validate_job_result, validate_progress, validate_reference_manifest,
            validate_workflow_summary,
        },
        verification::VerificationReport,
    };
    use serde::{Deserialize, Serialize};

    /// How many recent commits the published timeline carries.
    const PUBLISHED_COMMITS: usize = 20;

    /// The evidence directory a passing Verify job declares.
    const NATIVE_EVIDENCE: &str = "verification";

    /// The evidence directory a passing Build web job declares.
    const WEB_EVIDENCE: &str = "browser";

    /// The packaged game directory a passing Build web job uploads.
    const WEB_PACKAGE: &str = "play";

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
            publication: CurrentPublication,
            output: PathBuf,
        },
        Result {
            job: String,
            gates: PathBuf,
            output: PathBuf,
        },
        Inputs {
            repository: PathBuf,
            source_commit: String,
            run_url: String,
            native_outcome: String,
            web_outcome: String,
            native: Option<PathBuf>,
            web: Option<PathBuf>,
            previous: Option<PathBuf>,
            output: PathBuf,
        },
    }

    pub fn run() -> ExitCode {
        let command = match parse_command(env::args().skip(1)) {
            Ok(command) => command,
            Err(message) => {
                eprintln!("{message}");
                eprintln!("usage: sitegen <validate|build|assemble|result|inputs> [options]");
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
                publication,
                output,
            } => assemble(previous.as_deref(), &current, &result, publication, &output),
            Command::Result { job, gates, output } => result(&job, &gates, &output),
            Command::Inputs {
                repository,
                source_commit,
                run_url,
                native_outcome,
                web_outcome,
                native,
                web,
                previous,
                output,
            } => inputs(
                &repository,
                &source_commit,
                &run_url,
                &native_outcome,
                &web_outcome,
                native.as_deref(),
                web.as_deref(),
                previous.as_deref(),
                &output,
            ),
        }
    }

    fn parse_command(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
        let mut args = args.into_iter();
        let name = args.next().ok_or_else(|| "missing command".to_owned())?;
        let remaining = args.collect::<Vec<_>>();

        match name.as_str() {
            "validate" => {
                let values =
                    parse_options(&remaining, &["--progress", "--plan", "--repository"], &[])?;
                Ok(Command::Validate {
                    progress: values.required_path("--progress")?,
                    plan: values.required_path("--plan")?,
                    repository: values.required_path("--repository")?,
                })
            }
            "build" => {
                let values = parse_options(&remaining, &["--inputs", "--output"], &[])?;
                Ok(Command::Build {
                    inputs: values.required_path("--inputs")?,
                    output: values.required_path("--output")?,
                })
            }
            "assemble" => {
                let values = parse_options(
                    &remaining,
                    &["--current", "--result", "--output"],
                    &["--previous", "--publication"],
                )?;
                let publication = match values.optional("--publication") {
                    Some(value) => CurrentPublication::parse(&value).ok_or_else(|| {
                        format!(
                            "--publication must be {} or {}, not {value:?}",
                            CurrentPublication::GENERATED,
                            CurrentPublication::DEGRADED
                        )
                    })?,
                    None => CurrentPublication::Generated,
                };
                Ok(Command::Assemble {
                    previous: values.optional_path("--previous"),
                    current: values.required_path("--current")?,
                    result: values.required_path("--result")?,
                    publication,
                    output: values.required_path("--output")?,
                })
            }
            "result" => {
                let values = parse_options(&remaining, &["--job", "--gates", "--output"], &[])?;
                Ok(Command::Result {
                    job: values.required("--job")?,
                    gates: values.required_path("--gates")?,
                    output: values.required_path("--output")?,
                })
            }
            "inputs" => {
                let values = parse_options(
                    &remaining,
                    &[
                        "--repository",
                        "--source-commit",
                        "--run-url",
                        "--native-outcome",
                        "--web-outcome",
                        "--output",
                    ],
                    &["--native", "--web", "--previous"],
                )?;
                Ok(Command::Inputs {
                    repository: values.required_path("--repository")?,
                    source_commit: values.required("--source-commit")?,
                    run_url: values.required("--run-url")?,
                    native_outcome: values.required("--native-outcome")?,
                    web_outcome: values.required("--web-outcome")?,
                    native: values.optional_path("--native"),
                    web: values.optional_path("--web"),
                    previous: values.optional_path("--previous"),
                    output: values.required_path("--output")?,
                })
            }
            _ => Err(format!("unknown command: {name}")),
        }
    }

    struct ParsedOptions {
        values: Vec<(String, String)>,
    }

    impl ParsedOptions {
        fn required(&self, name: &str) -> Result<String, String> {
            self.optional(name)
                .ok_or_else(|| format!("missing required option {name}"))
        }

        fn optional(&self, name: &str) -> Option<String> {
            self.values
                .iter()
                .find(|(option, _)| option == name)
                .map(|(_, value)| value.clone())
        }

        fn required_path(&self, name: &str) -> Result<PathBuf, String> {
            self.required(name).map(PathBuf::from)
        }

        /// An option a caller may leave out, and may also pass empty.
        ///
        /// The workflow builds these arguments from step outcomes, so an
        /// absent artifact arrives as an empty string rather than as a missing
        /// option. Both mean the same thing and neither may become the current
        /// directory.
        fn optional_path(&self, name: &str) -> Option<PathBuf> {
            self.optional(name)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
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
                .any(|(existing, _): &(String, String)| existing == option)
            {
                return Err(format!("duplicate option: {option}"));
            }
            values.push((option.to_owned(), pair[1].clone()));
        }

        let parsed = ParsedOptions { values };
        for option in required {
            if parsed.optional(option).is_none() {
                return Err(format!("missing required option {option}"));
            }
        }
        Ok(parsed)
    }

    #[derive(Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct SiteInputPaths {
        progress: PathBuf,
        plan: PathBuf,
        reference_manifest: PathBuf,
        workflow: PathBuf,
        repo: PathBuf,
        verification: Option<VerificationInput>,
        gallery: Option<PathBuf>,
        playable: Option<PlayableInput>,
    }

    /// Where the raw verification documents and their artifacts live.
    #[derive(Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct VerificationInput {
        report: PathBuf,
        artifacts: PathBuf,
        browser: Option<BrowserInput>,
    }

    /// Where the raw browser gate summary and its diagnostics live.
    #[derive(Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct BrowserInput {
        report: PathBuf,
        artifacts: PathBuf,
    }

    #[derive(Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct PlayableInput {
        directory: PathBuf,
        source_commit: String,
        run_url: String,
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
            Some(input) => match project_verification(root, &input) {
                Ok(evidence) => Some(evidence),
                Err(message) => return content_error(message),
            },
            None => None,
        };
        let gallery = match paths.gallery {
            Some(path) => match read_json::<GalleryManifest>(&root.join(path)) {
                Ok(value) => Some(value),
                Err(message) => return content_error(message),
            },
            None => None,
        };
        let playable = paths.playable.map(|playable| PlayableBuild {
            directory: root.join(playable.directory),
            source_commit: playable.source_commit,
            run_url: playable.run_url,
        });
        let inputs = SiteInputs {
            progress,
            plan_markdown,
            reference_manifest,
            verification,
            gallery,
            workflow,
            repo,
            playable,
        };

        match build_site(&inputs, output) {
            Ok(manifest) => {
                println!("{}", manifest.source_commit);
                ExitCode::SUCCESS
            }
            Err(error) => content_error(error.to_string()),
        }
    }

    /// Reads the raw verification documents and projects them onto the public
    /// shape the generator publishes.
    fn project_verification(
        root: &Path,
        input: &VerificationInput,
    ) -> Result<VerificationEvidence, String> {
        let report = read_json::<VerificationReport>(&root.join(&input.report))?;
        let artifacts = root.join(&input.artifacts);
        let browser = match &input.browser {
            Some(browser) => Some((
                read_json::<BrowserGateReport>(&root.join(&browser.report))?,
                root.join(&browser.artifacts),
            )),
            None => None,
        };
        VerificationEvidence::project(
            &report,
            &artifacts,
            browser
                .as_ref()
                .map(|(gate, directory)| (gate, directory.as_path())),
        )
        .map_err(|error| error.to_string())
    }

    fn assemble(
        previous: Option<&Path>,
        current: &Path,
        result_path: &Path,
        publication: CurrentPublication,
        output: &Path,
    ) -> ExitCode {
        let workflow = match read_json::<WorkflowSummary>(result_path) {
            Ok(value) => value,
            Err(message) => return content_error(message),
        };
        if let Err(error) = validate_workflow_summary(&workflow) {
            return content_error(error.to_string());
        }
        match assemble_site(previous, current, &workflow, publication, output) {
            Ok(disposition) => {
                println!("{disposition}");
                ExitCode::SUCCESS
            }
            Err(error) => content_error(error.to_string()),
        }
    }

    // -----------------------------------------------------------------------
    // Workflow result manifests
    // -----------------------------------------------------------------------

    /// Turns one job's measured gates and produced artifacts into the strict
    /// result manifest Publish reads.
    ///
    /// This never fails on a failed gate: a job that failed still has to
    /// publish what it measured, and the workflow reaches its own verdict from
    /// the manifest afterwards.
    fn result(job: &str, gates_path: &Path, output: &Path) -> ExitCode {
        let lines = match fs::read_to_string(gates_path) {
            Ok(value) => value,
            // A job that fell over before its first gate leaves no file at
            // all. That is a failed job with no measurements, not a crash.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return content_error(format!("{}: {error}", gates_path.display())),
        };
        let gates = match read_gate_records(&lines) {
            Ok(gates) => gates,
            Err(error) => return content_error(error.to_string()),
        };
        let status = gate_verdict(&gates);
        let evidence = declared_evidence(job, output);
        let manifest = JobResult {
            job: job.to_owned(),
            status,
            gates,
            evidence,
        };
        if let Err(error) = validate_job_result(&manifest) {
            return content_error(error.to_string());
        }

        let json = match serde_json::to_string_pretty(&manifest) {
            Ok(json) => json,
            Err(error) => return content_error(error.to_string()),
        };
        if let Err(error) = fs::create_dir_all(output) {
            return content_error(format!("{}: {error}", output.display()));
        }
        let path = output.join(RESULT_FILE);
        if let Err(error) = fs::write(&path, json.as_bytes()) {
            return content_error(format!("{}: {error}", path.display()));
        }

        println!("{}", status_name(status));
        ExitCode::SUCCESS
    }

    /// The evidence directory a job may declare, when the directory really
    /// holds a complete, readable set.
    ///
    /// Declaring evidence is a promise Publish acts on, so the promise is
    /// checked here against the game's and the browser gate's own strict
    /// schemas rather than against the presence of a file name.
    fn declared_evidence(job: &str, root: &Path) -> Option<String> {
        match job {
            "verify" => {
                let directory = root.join(NATIVE_EVIDENCE);
                let report = read_json::<VerificationReport>(&directory.join("report.json"))
                    .inspect_err(|message| eprintln!("no publishable native evidence: {message}"))
                    .ok()?;
                VerificationEvidence::project(&report, &directory, None)
                    .inspect_err(|error| eprintln!("no publishable native evidence: {error}"))
                    .ok()?;
                Some(NATIVE_EVIDENCE.to_owned())
            }
            "build-web" => {
                let directory = root.join(WEB_EVIDENCE);
                read_json::<BrowserGateReport>(&directory.join("browser-gate.json"))
                    .inspect_err(|message| eprintln!("no publishable browser evidence: {message}"))
                    .ok()?;
                Some(WEB_EVIDENCE.to_owned())
            }
            _ => None,
        }
    }

    fn status_name(status: GateStatus) -> &'static str {
        match status {
            GateStatus::Passed => "passed",
            GateStatus::Failed => "failed",
            GateStatus::SkippedDependency => "skipped_dependency",
        }
    }

    // -----------------------------------------------------------------------
    // Publication inputs
    // -----------------------------------------------------------------------

    /// Decides everything Publish publishes, from what the two jobs left
    /// behind.
    ///
    /// Publish always runs, so every gap has to become a published fact: a
    /// missing result artifact, an unprojectable report, an incomplete package,
    /// and an absent previous publication are all decided here and written into
    /// one `inputs.json` the generator can read without knowing any of it.
    // Every argument is one named command-line option. Grouping them into a
    // struct would only move the same list one level away from the parser that
    // fills it.
    #[allow(clippy::too_many_arguments)]
    fn inputs(
        repository: &Path,
        source_commit: &str,
        run_url: &str,
        native_outcome: &str,
        web_outcome: &str,
        native: Option<&Path>,
        web: Option<&Path>,
        previous: Option<&Path>,
        output: &Path,
    ) -> ExitCode {
        if let Err(error) = fs::create_dir_all(output) {
            return content_error(format!("{}: {error}", output.display()));
        }

        let native_report = read_job_report(native, native_outcome);
        let web_report = read_job_report(web, web_outcome);
        let mut extra = Vec::new();

        let verification = native_evidence(native, &native_report, web, &web_report, &mut extra);
        let playable = playable_input(web, &web_report, source_commit, run_url, &mut extra);

        let mut workflow =
            match merge_job_results(source_commit, run_url, &native_report, &web_report) {
                Ok(workflow) => workflow,
                Err(error) => return content_error(error.to_string()),
            };
        workflow.gates.extend(extra);
        if let Err(error) = validate_workflow_summary(&workflow) {
            return content_error(error.to_string());
        }
        if let Err(message) = write_json(&output.join("workflow.json"), &workflow) {
            return content_error(message);
        }

        let repo = match read_repo_facts(repository) {
            Ok(repo) => repo,
            Err(message) => return content_error(message),
        };
        if let Err(message) = write_json(&output.join("repo.json"), &repo) {
            return content_error(message);
        }

        let gallery = inherited_gallery(previous);
        if let Err(message) = write_json(&output.join("gallery.json"), &gallery) {
            return content_error(message);
        }

        let paths = SiteInputPaths {
            progress: repository.join("docs/progress.json"),
            plan: repository.join("docs/implementation-plan.md"),
            reference_manifest: repository.join("docs/reference/manifest.json"),
            workflow: PathBuf::from("workflow.json"),
            repo: PathBuf::from("repo.json"),
            verification,
            gallery: Some(PathBuf::from("gallery.json")),
            playable,
        };
        if let Err(message) = write_json(&output.join("inputs.json"), &paths) {
            return content_error(message);
        }

        println!(
            "native={} web={}",
            status_name(workflow.native),
            status_name(workflow.web)
        );
        ExitCode::SUCCESS
    }

    /// One job's result artifact, read strictly or read as absent.
    ///
    /// A manifest that is missing, unreadable, or carries a value the site may
    /// not publish is treated exactly like a job that never uploaded one, so a
    /// malformed manifest can never publish itself.
    fn read_job_report(root: Option<&Path>, outcome: &str) -> JobReport {
        let outcome = JobOutcome::parse(outcome);
        let Some(root) = root else {
            return JobReport::absent(outcome);
        };
        let path = root.join(RESULT_FILE);
        match read_json::<JobResult>(&path).and_then(|result| {
            validate_job_result(&result)
                .map(|()| result)
                .map_err(|error| error.to_string())
        }) {
            Ok(result) => JobReport {
                outcome,
                result: Some(result),
            },
            Err(message) => {
                eprintln!("unusable {} result manifest: {message}", outcome.name());
                JobReport::absent(outcome)
            }
        }
    }

    /// The verification block Publish hands the generator, when both jobs left
    /// evidence the generator can really project.
    ///
    /// A native report that no longer projects is published as a failed gate
    /// rather than as a crash, because Publish has to publish the current
    /// status even when the evidence behind it is unusable.
    fn native_evidence(
        native_root: Option<&Path>,
        native: &JobReport,
        web_root: Option<&Path>,
        web: &JobReport,
        extra: &mut Vec<GateSummary>,
    ) -> Option<VerificationInput> {
        let directory = native_root?.join(native.evidence()?);
        let browser = web_root
            .zip(web.evidence())
            .map(|(root, evidence)| root.join(evidence))
            .filter(|directory| directory.join("browser-gate.json").is_file())
            .map(|directory| BrowserInput {
                report: directory.join("browser-gate.json"),
                artifacts: directory,
            });
        let input = VerificationInput {
            report: directory.join("report.json"),
            artifacts: directory,
            browser,
        };

        match project_verification(Path::new("."), &input) {
            Ok(_) => Some(input),
            Err(message) => {
                eprintln!("the declared verification evidence does not project: {message}");
                extra.push(failed_gate("Published verification evidence"));
                None
            }
        }
    }

    /// The packaged game Publish promotes, when the web job really proved one.
    fn playable_input(
        web_root: Option<&Path>,
        web: &JobReport,
        source_commit: &str,
        run_url: &str,
        extra: &mut Vec<GateSummary>,
    ) -> Option<PlayableInput> {
        if web.status() != GateStatus::Passed {
            return None;
        }
        let directory = web_root?.join(WEB_PACKAGE);
        let missing = missing_playable_parts(&directory);
        if !missing.is_empty() {
            eprintln!(
                "the packaged game is incomplete: missing {}",
                missing.join(", ")
            );
            extra.push(failed_gate("Published playable package"));
            return None;
        }
        Some(PlayableInput {
            directory,
            source_commit: source_commit.to_owned(),
            run_url: run_url.to_owned(),
        })
    }

    /// The screenshot history this build inherits.
    ///
    /// Only a previous `pages-live` publication carries one, so a first run
    /// starts from an explicitly empty history rather than from nothing.
    fn inherited_gallery(previous: Option<&Path>) -> GalleryManifest {
        let Some(path) = previous.map(|previous| previous.join("gallery.json")) else {
            return GalleryManifest::default();
        };
        match read_json::<GalleryManifest>(&path) {
            Ok(gallery) => gallery,
            Err(message) => {
                if path.exists() {
                    eprintln!("the inherited history is unusable: {message}");
                }
                GalleryManifest::default()
            }
        }
    }

    fn failed_gate(name: &str) -> GateSummary {
        GateSummary {
            name: name.to_owned(),
            status: GateStatus::Failed,
            passed: 0,
            failed: 1,
            duration_ms: 0,
            artifact_url: None,
        }
    }

    fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
        let json = serde_json::to_string_pretty(value)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        fs::write(path, json.as_bytes()).map_err(|error| format!("{}: {error}", path.display()))
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

        if let Err(errors) =
            validate_progress(&progress, &plan_task_ids_from_markdown(&plan), &repo)
        {
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
        let json =
            fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
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
            commits: read_commit_summaries(repository)?,
        })
    }

    /// The most recent commits, newest first, with nothing but what the
    /// timeline publishes.
    ///
    /// The subject is the only free text a commit contributes, and the site
    /// escapes it; nothing else about the repository or the machine is read.
    fn read_commit_summaries(repository: &Path) -> Result<Vec<CommitSummary>, String> {
        let separator = '\u{1f}';
        let log = git_output(
            repository,
            &[
                "log",
                "--max-count",
                &PUBLISHED_COMMITS.to_string(),
                &format!("--format=%H{separator}%cI{separator}%s"),
            ],
        )?;
        Ok(log
            .lines()
            .filter_map(|line| {
                let mut fields = line.splitn(3, separator);
                Some(CommitSummary {
                    sha: fields.next()?.to_owned(),
                    committed_at: fields.next()?.to_owned(),
                    subject: fields.next()?.to_owned(),
                    task_id: None,
                })
            })
            .collect())
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
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> std::process::ExitCode {
    native::run()
}

#[cfg(target_arch = "wasm32")]
fn main() {}
