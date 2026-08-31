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
        time::{SystemTime, UNIX_EPOCH},
    };

    use midcreek_cs_1::{
        sitegen::{
            BrowserGateReport, CommitSummary, GalleryManifest, GateStatus, GateSummary, JobOutcome,
            JobReport, JobResult, PlayableBuild, ProgressDocument, ProgressStatus, REPOSITORY_URL,
            RESULT_FILE, ReferenceManifest, RepoFacts, SiteInputs, VerificationEvidence,
            WorkflowSummary, assemble_site_in, build_site_in, check_readme_status,
            default_repository, gate_verdict, merge_job_results, missing_playable_parts,
            plan_task_ids_from_markdown, read_gate_records, render_readme_status,
            replace_readme_status, validate_job_result, validate_progress,
            validate_reference_manifest, validate_workflow_summary,
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
        Check {
            repository: PathBuf,
        },
        Readme {
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
                eprintln!(
                    "usage: sitegen <validate|check|readme|build|assemble|result|inputs> [options]"
                );
                return ExitCode::from(2);
            }
        };

        match command {
            Command::Validate {
                progress,
                plan,
                repository,
            } => validate(&progress, &plan, &repository),
            Command::Check { repository } => check(&repository),
            Command::Readme { repository } => readme(&repository),
            Command::Build { inputs, output } => build(&inputs, &output),
            Command::Assemble {
                previous,
                current,
                result,
                output,
            } => assemble(previous.as_deref(), &current, &result, &output),
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
            "check" => {
                let values = parse_options(&remaining, &["--repository"], &[])?;
                Ok(Command::Check {
                    repository: values.required_path("--repository")?,
                })
            }
            "readme" => {
                let values = parse_options(&remaining, &["--repository"], &[])?;
                Ok(Command::Readme {
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
                    &["--previous"],
                )?;
                Ok(Command::Assemble {
                    previous: values.optional_path("--previous"),
                    current: values.required_path("--current")?,
                    result: values.required_path("--result")?,
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
        /// The checkout the site is published from.
        ///
        /// The generator reads the approved references out of it, trusts its
        /// build root, and refuses to publish into its source tree. Publish
        /// declares it so a relocated `sitegen` still knows which repository
        /// it is generating for; an inputs document that leaves it out falls
        /// back to the checkout the binary was built in.
        repository: Option<PathBuf>,
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
        let repository = paths
            .repository
            .map_or_else(runtime_repository, |declared| root.join(declared));
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

        match build_site_in(&repository, &inputs, output) {
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
        output: &Path,
    ) -> ExitCode {
        let workflow = match read_json::<WorkflowSummary>(result_path) {
            Ok(value) => value,
            Err(message) => return content_error(message),
        };
        if let Err(error) = validate_workflow_summary(&workflow) {
            return content_error(error.to_string());
        }
        match assemble_site_in(&runtime_repository(), previous, current, &workflow, output) {
            Ok(disposition) => {
                println!("{disposition}");
                ExitCode::SUCCESS
            }
            Err(error) => content_error(error.to_string()),
        }
    }

    /// The checkout this invocation is really running against.
    ///
    /// Nothing is read out of it: it is the source tree the output may not be
    /// written into. The compiled-in path is the last resort because a
    /// relocated binary's is not on the machine at all, and a run that can name
    /// no source tree refuses to publish rather than publishing anywhere. The
    /// workflow declares nothing extra for this: a job that checked the
    /// repository out runs inside the checkout, or exports `GITHUB_WORKSPACE`
    /// when its working directory is elsewhere.
    ///
    /// `GITHUB_WORKSPACE` is a hint, not an assertion, so an unusable one
    /// falls through to discovery instead of replacing it. Discovery from the
    /// working directory runs first, because a valid but stale exported hint
    /// must not override the checkout that contains the command. The empty
    /// string is another unsafe hint: it names no directory at all, yet
    /// `"".join(".git")` is `.git`, which resolves against the working
    /// directory. A hint is therefore only taken when it is absolute, is a
    /// directory, and really holds a `.git` entry. Every accepted checkout is
    /// reported canonically.
    fn runtime_repository() -> PathBuf {
        env::current_dir()
            .ok()
            .and_then(|working| fs::canonicalize(working).ok())
            .and_then(|working| working.ancestors().find_map(checkout_root))
            .or_else(|| {
                env::var_os("GITHUB_WORKSPACE")
                    .map(PathBuf::from)
                    .filter(|workspace| workspace.is_absolute() && workspace.is_dir())
                    .and_then(|workspace| checkout_root(&workspace))
            })
            .unwrap_or_else(default_repository)
    }

    /// The canonical path of one candidate directory that really is a
    /// checkout.
    ///
    /// A `.git` entry is enough and is not required to be a directory: a
    /// worktree and a submodule both carry a `.git` file naming their real
    /// git directory, and both are checkouts an output may not be written
    /// into.
    fn checkout_root(candidate: &Path) -> Option<PathBuf> {
        candidate
            .join(".git")
            .exists()
            .then(|| fs::canonicalize(candidate).ok())
            .flatten()
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

        // Every path this document publishes is built from the repository
        // root, and `build` resolves the document's own relative paths against
        // the directory the document lives in. A relative `--repository` would
        // therefore be reinterpreted against the output directory later, so it
        // is resolved once, here, against the working directory it was really
        // meant for.
        let repository = &match fs::canonicalize(repository) {
            Ok(resolved) => resolved,
            Err(error) => {
                return content_error(format!("{}: {error}", repository.display()));
            }
        };

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

        // The site resolves exactly the commits the progress document it
        // publishes names, so that document decides which commits have to be
        // looked up at all. It is the document this run is about to publish:
        // one that cannot be read, or that does not match the schema, is a
        // failure of this run rather than a run with no references, because
        // publishing it is exactly what happens next.
        let progress_path = repository.join(PROGRESS_DOCUMENT);
        let published_progress = match read_json::<ProgressDocument>(&progress_path) {
            Ok(progress) => referenced_commits(&progress),
            Err(message) => return content_error(message),
        };
        let repo = match read_repo_facts(repository, &published_progress) {
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
            repository: Some(repository.to_path_buf()),
            progress: progress_path,
            plan: repository.join(PLAN_DOCUMENT),
            reference_manifest: repository.join(REFERENCE_MANIFEST),
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
    /// status even when the evidence behind it is unusable. The two halves are
    /// judged separately: an unreadable browser canvas is the browser gate's
    /// failure alone, and losing fourteen verified native frames over it would
    /// throw away evidence that is still exactly as good as it was.
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

        let message = match project_verification(Path::new("."), &input) {
            Ok(_) => return Some(input),
            Err(message) => message,
        };
        if input.browser.is_some() {
            let native_only = VerificationInput {
                report: input.report.clone(),
                artifacts: input.artifacts.clone(),
                browser: None,
            };
            if project_verification(Path::new("."), &native_only).is_ok() {
                eprintln!("the declared browser evidence does not project: {message}");
                extra.push(failed_gate("Published browser evidence"));
                return Some(native_only);
            }
        }
        eprintln!("the declared verification evidence does not project: {message}");
        extra.push(failed_gate("Published verification evidence"));
        None
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
        let repo = match read_repo_facts(repository, &referenced_commits(&progress)) {
            Ok(repo) => repo,
            Err(message) => return content_error(message),
        };
        let reference_manifest_path = repository.join(REFERENCE_MANIFEST);
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

    // -----------------------------------------------------------------------
    // The whole-repository gate and the README block it maintains
    // -----------------------------------------------------------------------

    /// The canonical progress document every command reads.
    const PROGRESS_DOCUMENT: &str = "docs/progress.json";

    /// The reviewed implementation plan progress task IDs are declared in.
    const PLAN_DOCUMENT: &str = "docs/implementation-plan.md";

    /// The approved reference provenance the site publishes.
    const REFERENCE_MANIFEST: &str = "docs/reference/manifest.json";

    /// The README carrying the one generated status block.
    const README_DOCUMENT: &str = "README.md";

    /// Proves one checkout is publishable, from its own canonical sources
    /// alone.
    ///
    /// Everything the gate judges is read from `repository`: the progress
    /// document, the reviewed plan, the approved reference manifest, the
    /// README, and the checkout's own Git facts. The path is canonicalized
    /// first, so the verdict is the same from any working directory. Nothing
    /// is written into the checkout — the site the gate has to generate to
    /// judge it is built into a temporary directory that is removed however
    /// the run ends — so `check` is safe to run on a tree somebody is editing.
    ///
    /// Every source-level failure is reported before the gate gives up, so one
    /// run names everything that has to be fixed rather than one thing at a
    /// time. The generated site is only attempted once the sources agree,
    /// because `build_site_in` re-validates them and would otherwise repeat
    /// the same failures in a less useful form.
    fn check(repository: &Path) -> ExitCode {
        let sources = match CanonicalSources::read(repository) {
            Ok(sources) => sources,
            Err(message) => return content_error(message),
        };
        let reference_path = sources.repository.join(REFERENCE_MANIFEST);
        let reference_manifest = match read_json::<ReferenceManifest>(&reference_path) {
            Ok(manifest) => manifest,
            Err(message) => return content_error(message),
        };
        let repo =
            match read_repo_facts(&sources.repository, &referenced_commits(&sources.progress)) {
                Ok(repo) => repo,
                Err(message) => return content_error(message),
            };

        let mut refused = false;
        if let Err(errors) = validate_progress(
            &sources.progress,
            &plan_task_ids_from_markdown(&sources.plan_markdown),
            &repo,
        ) {
            refused = true;
            for error in errors {
                eprintln!("{}: {error}", sources.progress_path.display());
            }
        }
        if let Err(errors) = validate_reference_manifest(&reference_manifest, &sources.repository) {
            refused = true;
            for error in errors {
                eprintln!("{}: {error}", reference_path.display());
            }
        }
        if let Err(error) = check_readme_status(&sources.readme, &sources.readme_block()) {
            refused = true;
            eprintln!("{}: {error}", sources.readme_path.display());
        }
        if refused {
            return ExitCode::from(1);
        }

        let scratch = match ScratchSite::new() {
            Ok(scratch) => scratch,
            Err(message) => return content_error(message),
        };
        let current = sources
            .progress
            .tasks
            .iter()
            .find(|task| task.status == ProgressStatus::InProgress)
            .map_or("all-done", |task| task.id.as_str())
            .to_owned();
        let inputs = SiteInputs {
            progress: sources.progress,
            plan_markdown: sources.plan_markdown,
            reference_manifest,
            verification: None,
            gallery: None,
            // A local gate proves nothing about a workflow run, so it claims
            // nothing about one: both jobs are published as not run, and the
            // only URL the page carries is this repository's own run index.
            workflow: WorkflowSummary {
                source_commit: repo.head_sha.clone(),
                run_url: format!("{REPOSITORY_URL}/actions"),
                native: GateStatus::SkippedDependency,
                web: GateStatus::SkippedDependency,
                gates: Vec::new(),
            },
            repo,
            playable: None,
        };
        if let Err(error) = build_site_in(&sources.repository, &inputs, scratch.path()) {
            return content_error(error.to_string());
        }

        println!("{current}");
        ExitCode::SUCCESS
    }

    /// Rewrites the one generated README status block and nothing else.
    ///
    /// This is the only command that edits a checkout, and it edits exactly
    /// the bytes between the two delimiters. A README whose block is missing,
    /// duplicated, unmatched, or inverted is refused rather than repaired,
    /// because every way of choosing a span in such a file rewrites bytes a
    /// person wrote. `check` stays read-only; this is what makes its verdict
    /// actionable.
    fn readme(repository: &Path) -> ExitCode {
        let sources = match CanonicalSources::read(repository) {
            Ok(sources) => sources,
            Err(message) => return content_error(message),
        };
        let updated = match replace_readme_status(&sources.readme, &sources.readme_block()) {
            Ok(updated) => updated,
            Err(error) => {
                return content_error(format!("{}: {error}", sources.readme_path.display()));
            }
        };
        if updated == sources.readme {
            println!("unchanged");
            return ExitCode::SUCCESS;
        }
        if let Err(message) = replace_file(&sources.readme_path, &updated) {
            return content_error(message);
        }

        println!("updated");
        ExitCode::SUCCESS
    }

    /// The canonical documents both repository-wide commands read, together
    /// with the stored bytes each was read from.
    ///
    /// The repository is canonicalized once, here, so every path below it is
    /// absolute and the same wherever the command was launched from. The
    /// progress document is kept both parsed and as stored, because the
    /// generated README block is a function of both: what the document says,
    /// and what its bytes are.
    struct CanonicalSources {
        repository: PathBuf,
        progress_path: PathBuf,
        progress_json: String,
        progress: ProgressDocument,
        plan_markdown: String,
        readme_path: PathBuf,
        readme: String,
    }

    impl CanonicalSources {
        fn read(repository: &Path) -> Result<Self, String> {
            let repository = fs::canonicalize(repository)
                .map_err(|error| format!("{}: {error}", repository.display()))?;
            let progress_path = repository.join(PROGRESS_DOCUMENT);
            let progress_json = read_text(&progress_path)?;
            let progress = serde_json::from_str::<ProgressDocument>(&progress_json)
                .map_err(|error| format!("{}: {error}", progress_path.display()))?;
            let plan_markdown = read_text(&repository.join(PLAN_DOCUMENT))?;
            let readme_path = repository.join(README_DOCUMENT);
            let readme = read_text(&readme_path)?;
            Ok(Self {
                repository,
                progress_path,
                progress_json,
                progress,
                plan_markdown,
                readme_path,
                readme,
            })
        }

        /// The one generated status block these sources produce.
        fn readme_block(&self) -> String {
            render_readme_status(&self.progress, &self.progress_json, &self.plan_markdown)
        }
    }

    fn read_text(path: &Path) -> Result<String, String> {
        fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
    }

    /// A directory outside every checkout that one `check` builds its site
    /// into.
    ///
    /// The directory is removed however the run ends — a refused page, an
    /// unreadable source, a panic — so a gate that has to generate a whole
    /// site never grows the temporary directory of the machine that ran it.
    struct ScratchSite(PathBuf);

    impl ScratchSite {
        fn new() -> Result<Self, String> {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos();
            Ok(Self(env::temp_dir().join(format!(
                "sitegen-check-{}-{unique}",
                std::process::id()
            ))))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for ScratchSite {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Replaces one file's contents in a single step.
    ///
    /// The new bytes are written to a sibling and renamed over the file, so a
    /// run interrupted at any point leaves the original exactly as it was
    /// rather than truncated or half written. A failed write removes its own
    /// sibling, so a refused run leaves nothing behind either.
    fn replace_file(path: &Path, contents: &str) -> Result<(), String> {
        let directory = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("{}: names no file to replace", path.display()))?;
        let pending = directory.join(format!(".{name}.sitegen-{}.tmp", std::process::id()));
        fs::write(&pending, contents.as_bytes()).map_err(|error| {
            let _ = fs::remove_file(&pending);
            format!("{}: {error}", pending.display())
        })?;
        fs::rename(&pending, path).map_err(|error| {
            let _ = fs::remove_file(&pending);
            format!("{}: {error}", path.display())
        })
    }

    fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
        let json = read_text(path)?;
        serde_json::from_str(&json).map_err(|error| format!("{}: {error}", path.display()))
    }

    /// The repository facts the site publishes, bounded by what they have to
    /// resolve.
    ///
    /// `known_commits` exists so the published documents' commit references
    /// resolve, and so the timeline renders. Enumerating the whole repository
    /// grew both the published facts and the work of collecting them with
    /// every commit anybody ever pushed, for commits nothing on the site will
    /// ever name. What is collected is therefore the head, the published
    /// timeline, and exactly the commits `referenced` names — each confirmed
    /// against the checkout one at a time, so an invented reference still
    /// fails validation.
    fn read_repo_facts(
        repository: &Path,
        referenced: &BTreeSet<String>,
    ) -> Result<RepoFacts, String> {
        let head_sha = git_output(repository, &["rev-parse", "HEAD"])?;
        let commits = read_commit_summaries(repository)?;
        let mut known_commits = commits
            .iter()
            .map(|commit| commit.sha.clone())
            .collect::<BTreeSet<_>>();
        known_commits.insert(head_sha.clone());
        for commit in referenced {
            if commit_exists(repository, commit) {
                known_commits.insert(commit.clone());
            }
        }
        Ok(RepoFacts {
            head_sha,
            known_commits,
            commits,
        })
    }

    /// Every commit one progress document names, as a full SHA.
    ///
    /// Symbolic references like `HEAD` are resolved by the generator itself
    /// and are not looked up here; anything that is not a full hexadecimal SHA
    /// never reaches Git.
    fn referenced_commits(progress: &ProgressDocument) -> BTreeSet<String> {
        progress
            .tasks
            .iter()
            .filter_map(|task| task.completed_commit.as_deref())
            .chain(
                progress
                    .challenges
                    .iter()
                    .filter_map(|challenge| challenge.resolved_commit.as_deref()),
            )
            .filter(|commit| {
                commit.len() == 40 && commit.chars().all(|value| value.is_ascii_hexdigit())
            })
            .map(str::to_owned)
            .collect()
    }

    /// Whether the checkout really holds one full commit SHA.
    fn commit_exists(repository: &Path, commit: &str) -> bool {
        git_output(
            repository,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{commit}^{{commit}}"),
            ],
        )
        .is_ok_and(|resolved| resolved == commit)
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
