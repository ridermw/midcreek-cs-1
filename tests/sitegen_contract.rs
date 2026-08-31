mod support;

use midcreek_cs_1::sitegen::{
    Challenge, ChallengeStatus, ProgressError, ProgressStatus, ReferenceError, ReferenceManifest,
    SitegenError, build_site, plan_task_ids_from_markdown, resolve_commit_ref,
    validate_output_path, validate_progress, validate_reference_manifest, validate_site_output,
};
use std::{
    fs,
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
use support::{
    assert_has_element_id, assert_text, build_fixture_site, build_site_from_inputs, fixture,
    plan_ids, read, repo_facts, sha256, site_inputs, validate_fixture,
};

mod generated_site_contract {
    use super::*;

    #[test]
    fn renders_every_required_section() {
        let output = build_fixture_site("green").unwrap();
        let html = output.index_html();

        for id in [
            "build-status",
            "play",
            "comparison",
            "progress",
            "screenshots",
            "plan",
            "challenges",
            "tests",
            "commits",
        ] {
            assert_has_element_id(&html, id);
        }
    }

    #[test]
    fn reports_that_no_verified_playable_build_exists() {
        let html = build_fixture_site("green").unwrap().index_html();

        assert_text(&html, "#play", "No verified playable build yet");
        assert!(!html.contains("<canvas"));
        assert!(!html.contains("<iframe"));
    }

    #[test]
    fn labels_the_current_source_when_verification_failed() {
        let mut inputs = site_inputs("green");
        inputs.workflow.native = midcreek_cs_1::sitegen::GateStatus::Failed;
        let html = build_site_from_inputs("failed-native", &inputs)
            .unwrap()
            .index_html();

        assert_text(&html, "#build-status", "CURRENT SOURCE: FAILED AT 11111111");
    }

    #[test]
    fn preserves_ascii_diagrams_as_preformatted_text() {
        let html = build_fixture_site("green").unwrap().index_html();

        assert!(html.contains("<pre><code class=\"language-text\">"));
        assert!(html.contains("main push"));
    }

    #[test]
    fn escapes_progress_and_commit_content() {
        let html = build_fixture_site("hostile-content").unwrap().index_html();

        assert!(!html.contains("<script>alert("));
        assert!(html.contains("&lt;script&gt;alert("));
    }

    #[test]
    fn renders_template_tokens_from_source_content_as_text() {
        let html = build_fixture_site("hostile-content").unwrap().index_html();

        assert!(html.contains("{{PLAY}}"));
        assert_has_element_id(&html, "play");
        assert_eq!(html.matches("No verified playable build yet").count(), 1);
    }

    #[test]
    fn comparison_images_are_accessible_and_copied_locally() {
        let site = build_fixture_site("green").unwrap();
        let html = site.index_html();
        let document = scraper::Html::parse_document(&html);
        let selector = scraper::Selector::parse("#comparison img").unwrap();
        let images = document.select(&selector).collect::<Vec<_>>();

        assert_eq!(images.len(), 2);
        for image in images {
            let alt = image.value().attr("alt").unwrap_or_default();
            let source = image.value().attr("src").unwrap_or_default();
            assert!(!alt.trim().is_empty());
            assert!(!source.starts_with('/'));
            assert!(
                site.root().join(source).is_file(),
                "{source} was not copied"
            );
        }
    }

    /// Task cards and challenge cards both publish commit links. They are
    /// built in different functions, so a repository rename applied to one and
    /// not the other publishes dead links from the other. One constant is the
    /// only thing either may name.
    #[test]
    fn every_published_commit_link_is_built_from_one_repository_url() {
        let html = build_fixture_site("verified-game").unwrap().index_html();
        let document = scraper::Html::parse_document(&html);
        let selector = scraper::Selector::parse("a[href*=\"/commit/\"]").unwrap();
        let links = document
            .select(&selector)
            .map(|link| link.value().attr("href").unwrap_or_default().to_owned())
            .collect::<Vec<_>>();
        let expected = format!("{}/commit/", midcreek_cs_1::sitegen::REPOSITORY_URL);

        assert_eq!(
            links.len(),
            3,
            "this fixture publishes two task commits and one resolved challenge: {links:?}"
        );
        assert!(
            document
                .select(&scraper::Selector::parse("#progress a[href*=\"/commit/\"]").unwrap())
                .count()
                >= 1,
            "the task cards must be one of the two link sources"
        );
        assert!(
            document
                .select(&scraper::Selector::parse("#challenges a[href*=\"/commit/\"]").unwrap())
                .count()
                >= 1,
            "the challenge cards must be the other link source"
        );
        for href in &links {
            assert!(
                href.starts_with(&expected),
                "{href} is not built from {expected}"
            );
        }
        assert!(
            midcreek_cs_1::sitegen::REPOSITORY_URL.starts_with("https://github.com/"),
            "the repository constant must stay on the one trusted host"
        );
    }

    #[test]
    fn progress_tasks_link_to_rendered_plan_headings() {
        let html = build_fixture_site("green").unwrap().index_html();
        let document = scraper::Html::parse_document(&html);
        let selector = scraper::Selector::parse("#progress a[href^=\"#plan-\"]").unwrap();
        let links = document.select(&selector).collect::<Vec<_>>();

        assert_eq!(links.len(), 3);
        for link in links {
            let target = link.value().attr("href").unwrap();
            assert_has_element_id(&html, target.trim_start_matches('#'));
        }
    }

    #[test]
    fn emits_only_declared_site_files() {
        let site = build_fixture_site("green").unwrap();
        let generated = &site.manifest().generated_files;

        assert_eq!(
            generated,
            &[
                Path::new("index.html").to_path_buf(),
                Path::new("reference/cel-shift-character-sheet.png").to_path_buf(),
                Path::new("reference/cel-shift-key-art.png").to_path_buf(),
                Path::new("site.css").to_path_buf(),
                Path::new("site.js").to_path_buf(),
            ]
        );
    }

    #[test]
    fn build_cli_renders_the_fixture_site() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let output_dir = std::env::temp_dir().join(format!(
            "midcreek-sitegen-cli-{}-{unique}",
            std::process::id()
        ));
        let output = Command::new(env!("CARGO_BIN_EXE_sitegen"))
            .current_dir(root)
            .args([
                "build",
                "--inputs",
                "tests/fixtures/sitegen/green/inputs.json",
                "--output",
                output_dir.to_str().unwrap(),
            ])
            .output()
            .expect("sitegen should launch");

        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_has_element_id(&read(output_dir.join("index.html")), "build-status");
        fs::remove_dir_all(output_dir).unwrap();
    }

    #[test]
    fn comparison_script_clamps_pointer_and_keyboard_updates() {
        let site = build_fixture_site("green").unwrap();
        let script = site.root().join("site.js");
        let harness = r#"
const fs = require("fs");
const listeners = {};
const properties = {};
const comparison = {
  style: { setProperty: (name, value) => { properties[name] = value; } }
};
const control = {
  value: "250",
  attributes: {},
  closest: () => comparison,
  addEventListener: (name, callback) => { listeners[name] = callback; },
  setAttribute: function(name, value) { this.attributes[name] = value; }
};
global.document = {
  querySelectorAll: (selector) =>
    selector === "[data-compare-control]" ? [control] : [],
  querySelector: () => null
};
global.window = {};
eval(fs.readFileSync(process.argv[1], "utf8"));
if (properties["--comparison"] !== "100%") process.exit(10);
if (control.attributes["aria-valuenow"] !== "100") process.exit(11);
control.value = "-20";
listeners.input();
if (properties["--comparison"] !== "0%") process.exit(12);
control.value = "40";
listeners.change();
if (properties["--comparison"] !== "40%") process.exit(13);
"#;
        let output = Command::new("node")
            .args(["-e", harness, script.to_str().unwrap()])
            .output()
            .expect("Node should execute the dependency-free site script");

        assert_eq!(
            output.status.code(),
            Some(0),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

mod output_validation_contract {
    use super::*;

    #[test]
    fn rejects_more_than_one_main_element() {
        let site = build_fixture_site("green").unwrap();
        mutate_index(&site, |html| {
            html.replace("</body>", "<main id=\"extra-main\"></main></body>")
        });

        let result = validate_site_output(site.root(), &fixture("green/progress.json"));
        assert!(
            matches!(result, Err(SitegenError::InvalidHtml { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn rejects_duplicate_element_ids() {
        let site = build_fixture_site("green").unwrap();
        mutate_index(&site, |html| {
            html.replace(
                "<section class=\"panel section-panel\" id=\"play\">",
                "<section class=\"panel section-panel\" id=\"build-status\">",
            )
        });

        let result = validate_site_output(site.root(), &fixture("green/progress.json"));
        assert!(
            matches!(result, Err(SitegenError::InvalidHtml { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn rejects_images_without_alt_text() {
        let site = build_fixture_site("green").unwrap();
        mutate_index(&site, |html| {
            html.replace("alt=\"Approved Cel Shift key art reference\"", "alt=\"\"")
        });

        assert_eq!(
            validate_site_output(site.root(), &fixture("green/progress.json")),
            Err(SitegenError::MissingAltText {
                source: Path::new("index.html").to_path_buf(),
            })
        );
    }

    #[test]
    fn rejects_missing_local_resources() {
        let site = build_fixture_site("green").unwrap();
        mutate_index(&site, |html| {
            html.replace("href=\"site.css\"", "href=\"missing.css\"")
        });

        assert_eq!(
            validate_site_output(site.root(), &fixture("green/progress.json")),
            Err(SitegenError::BrokenLocalLink {
                source: Path::new("index.html").to_path_buf(),
                target: Path::new("missing.css").to_path_buf(),
            })
        );
    }

    #[test]
    fn rejects_absolute_local_paths() {
        let site = build_fixture_site("green").unwrap();
        mutate_index(&site, |html| {
            html.replace("href=\"site.css\"", "href=\"/Users/example/site.css\"")
        });

        let result = validate_site_output(site.root(), &fixture("green/progress.json"));
        assert!(
            matches!(result, Err(SitegenError::InvalidHtml { .. })),
            "{result:?}"
        );
    }

    /// A published page is served from a URL, so the checkout it was published
    /// from never belongs in one — in rendered text, in an attribute, or
    /// anywhere else in the document. The rule names the repository and the
    /// output this publication was handed rather than guessing which
    /// absolute-looking prose is a leak.
    #[test]
    fn rejects_a_path_of_the_machine_that_published_the_page() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
        let leaks = [
            repository.join("docs/reference").display().to_string(),
            fs::canonicalize(repository).unwrap().display().to_string(),
            "file:///anywhere/at/all".to_owned(),
        ];

        for leak in leaks {
            for (label, mutation) in [
                ("rendered text", "Rendered from {leak}"),
                ("an attribute", r#"<span title="{leak}">from</span>"#),
            ] {
                let site = build_fixture_site("green").unwrap();
                let injected = mutation.replace("{leak}", &leak);
                mutate_index(&site, |html| {
                    html.replace("Render the progress hub", &injected)
                });

                // The public entry point declares the compiled-in checkout as
                // the repository, which is the one this build really published
                // from.
                let result = validate_site_output(site.root(), &fixture("green/progress.json"));
                assert!(
                    matches!(&result, Err(SitegenError::InvalidHtml { message, .. })
                        if message.contains("path of the publishing machine")),
                    "{leak} survived validation in {label}: {result:?}"
                );
            }
        }
    }

    /// The directory a page was published into is one of the two locations
    /// this publication was handed, and a build that names its own output is
    /// leaking the runner's layout just as surely as one that names the
    /// checkout. The directory that *encloses* the output is not: it is
    /// nothing this run chose, and refusing it would be refusing whatever
    /// shared root the host happens to hand out. This proves the output root
    /// itself is what decides, not the temporary root it sits under.
    #[test]
    fn rejects_the_output_directory_the_page_was_published_into() {
        let enclosing = std::env::temp_dir().display().to_string();
        let staged = build_fixture_site("green").unwrap();
        assert!(
            staged.root().starts_with(&enclosing),
            "this fixture publishes below {enclosing}"
        );
        mutate_index(&staged, |html| {
            html.replace(
                "Render the progress hub",
                &format!("Staged below {enclosing}"),
            )
        });
        assert_eq!(
            validate_site_output(staged.root(), &fixture("green/progress.json")),
            Ok(()),
            "the root the output happens to sit under is not this publication's own"
        );

        let site = build_fixture_site("green").unwrap();
        let output = site.root().display().to_string();
        mutate_index(&site, |html| {
            html.replace("Render the progress hub", &format!("Written to {output}"))
        });

        let result = validate_site_output(site.root(), &fixture("green/progress.json"));

        assert!(
            matches!(&result, Err(SitegenError::InvalidHtml { message, .. })
                if message.contains("path of the publishing machine")),
            "{result:?}"
        );
    }

    /// The repository a run publishes from is whatever root the caller named,
    /// and a shallow one is still a checkout. Skipping roots with too few
    /// components to look "identifying" was a guess about path shapes: `/srv`
    /// is an ordinary place to check a repository out, and a page that names
    /// it names the machine that built it.
    #[test]
    fn a_shallow_declared_repository_is_still_the_checkout_it_names() {
        for root in ["/srv", "/srv/hub"] {
            let site = build_fixture_site("green").unwrap();
            mutate_index(&site, |html| {
                html.replace(
                    "Render the progress hub",
                    &format!("Published from {root}/docs/reference"),
                )
            });

            let result = midcreek_cs_1::sitegen::validate_site_output_in(
                Path::new(root),
                site.root(),
                &fixture("green/progress.json"),
            );
            assert!(
                matches!(&result, Err(SitegenError::InvalidHtml { message, .. })
                    if message.contains("path of the publishing machine")),
                "{root} is the declared checkout: {result:?}"
            );
        }
    }

    #[test]
    fn a_path_prefix_sibling_is_not_the_declared_repository() {
        let site = build_fixture_site("green").unwrap();
        mutate_index(&site, |html| {
            html.replace(
                "Render the progress hub",
                "Published from /srv/hubris/docs/reference",
            )
        });

        assert_eq!(
            midcreek_cs_1::sitegen::validate_site_output_in(
                Path::new("/srv/hub"),
                site.root(),
                &fixture("green/progress.json"),
            ),
            Ok(())
        );
    }

    #[test]
    fn an_absolute_path_segment_is_not_the_declared_repository_root() {
        for text in [
            "Published from /mnt/srv/hub/docs/reference",
            "Published from https://example.invalid/srv/hub/docs/reference",
        ] {
            let site = build_fixture_site("green").unwrap();
            mutate_index(&site, |html| html.replace("Render the progress hub", text));

            assert_eq!(
                midcreek_cs_1::sitegen::validate_site_output_in(
                    Path::new("/srv/hub"),
                    site.root(),
                    &fixture("green/progress.json"),
                ),
                Ok(()),
                "{text} does not name the declared repository root"
            );
        }
    }

    /// The same bytes, checked against the same declared repository and
    /// output, must reach the same verdict on every machine and from every
    /// working directory. A gate that consulted the environment publishes a
    /// page on a laptop and refuses the identical page on the runner, which
    /// makes the check unreproducible and its failures unexplainable.
    ///
    /// The page below names this host's home directory, its temporary root,
    /// the working directory the suite runs in — which is also the checkout
    /// this binary was compiled in — and the two paths a GitHub runner
    /// exports. None of them is the repository or the output this publication
    /// was handed, so none of them decides anything.
    #[test]
    fn the_hosts_environment_never_decides_whether_a_page_is_publishable() {
        let foreign_checkout = "/home/runner/work/other-project/other-project";
        let mut ambient = vec![
            std::env::temp_dir().display().to_string(),
            std::env::current_dir().unwrap().display().to_string(),
            foreign_checkout.to_owned(),
            "/home/runner/_temp/other-project".to_owned(),
        ];
        ambient.extend(
            ["HOME", "USERPROFILE"]
                .into_iter()
                .filter_map(std::env::var_os)
                .map(|value| std::path::PathBuf::from(value).display().to_string()),
        );

        let site = build_fixture_site("green").unwrap();
        for (index, value) in ambient.iter().enumerate() {
            mutate_index(&site, |html| {
                html.replace(
                    "Render the progress hub",
                    &format!("Ambient {index}: {value}. Render the progress hub"),
                )
            });
        }

        // A repository that is not on this machine at all: only what the
        // caller declared can matter, so nothing has to resolve.
        let declared = std::env::temp_dir().join("midcreek-declared-checkout-elsewhere");
        assert_eq!(
            midcreek_cs_1::sitegen::validate_site_output_in(
                &declared,
                site.root(),
                &fixture("green/progress.json"),
            ),
            Ok(()),
            "no value the host happens to carry may decide a publication"
        );

        // The very same bytes are refused the moment the caller declares the
        // checkout they name: the verdict follows the declaration.
        let result = midcreek_cs_1::sitegen::validate_site_output_in(
            Path::new(foreign_checkout),
            site.root(),
            &fixture("green/progress.json"),
        );
        assert!(
            matches!(&result, Err(SitegenError::InvalidHtml { message, .. })
                if message.contains("path of the publishing machine")),
            "the declared repository decides, whichever machine it is on: {result:?}"
        );
    }

    /// The same rule may not fire on the relative paths the page publishes on
    /// purpose, on prose that merely contains a slash, or on the absolute
    /// paths the reserved plan, progress, and commit prose really contain. A
    /// guard tuned to what an absolute path looks like rejects a checked-in
    /// sentence about `/var/lib` and stops the whole publication for it.
    #[test]
    fn accepts_the_relative_paths_and_absolute_prose_the_page_really_publishes() {
        let site = build_fixture_site("verified-game").unwrap();
        let html = site.index_html();

        assert!(
            html.contains("docs/reference/cel-shift-key-art.png"),
            "{html}"
        );
        assert!(html.contains("../midcreek-concept/"), "{html}");
        assert!(html.contains("Key art / current frame"), "{html}");
        assert_eq!(
            validate_site_output(site.root(), &fixture("verified-game/progress.json")),
            Ok(())
        );

        // Each of these really appears in this project's own published prose
        // or is exactly the kind of sentence it may carry next: the URL prefix
        // the game is served below, a system path that belongs to no machine
        // in particular, and a documentation path.
        for prose in [
            "Served below the /midcreek-cs-1/ project prefix.",
            "The daemon keeps its state in /var/lib/midcreek.",
            "Installed alongside /usr/share/doc/midcreek/README.",
        ] {
            let site = build_fixture_site("verified-game").unwrap();
            mutate_index(&site, |html| {
                html.replace("Working on", &format!("{prose} Working on"))
            });
            assert_eq!(
                validate_site_output(site.root(), &fixture("verified-game/progress.json")),
                Ok(()),
                "{prose} is prose, not a leak"
            );
        }
    }

    /// Commit subjects are free text this repository writes and the page
    /// renders verbatim, so they are the most likely carrier of a real leak
    /// and of legitimate absolute prose alike. Both are decided here, on the
    /// same field, by whether the path belongs to the publishing machine.
    #[test]
    fn a_commit_subject_may_carry_prose_paths_but_never_the_checkout() {
        let leaked = format!(
            "fix: publish from {}",
            Path::new(env!("CARGO_MANIFEST_DIR")).display()
        );
        let mut inputs = site_inputs("green");
        assert!(
            !inputs.repo.commits.is_empty(),
            "this fixture publishes a commit timeline"
        );
        inputs.repo.commits[0].subject = leaked.clone();

        let result = build_site_from_inputs("commit-subject-leak", &inputs);
        assert!(
            matches!(&result.as_ref().err(), Some(SitegenError::InvalidHtml { message, .. })
                if message.contains("path of the publishing machine")),
            "{leaked} must never reach the page: {:?}",
            result.err()
        );

        inputs.repo.commits[0].subject =
            "fix: read the seed from /var/lib/midcreek/seed.json".to_owned();
        let published = build_site_from_inputs("commit-subject-prose", &inputs)
            .expect("an ordinary absolute path in a subject is not a leak")
            .index_html();
        assert!(
            published.contains("/var/lib/midcreek/seed.json"),
            "{published}"
        );

        // Another machine's checkout is prose too. It is not this run's
        // repository or output, and the page renders it exactly as written
        // wherever the page is validated.
        inputs.repo.commits[0].subject =
            "fix: mirror /home/runner/work/other-project/other-project/docs".to_owned();
        let published = build_site_from_inputs("commit-subject-foreign", &inputs)
            .expect("a foreign checkout path in a subject is not this publication's")
            .index_html();
        assert!(
            published.contains("/home/runner/work/other-project/other-project/docs"),
            "{published}"
        );
    }

    #[test]
    fn rejects_inline_script_content() {
        let site = build_fixture_site("green").unwrap();
        mutate_index(&site, |html| {
            html.replace("</body>", "<script>alert(1)</script></body>")
        });

        let result = validate_site_output(site.root(), &fixture("green/progress.json"));
        assert!(
            matches!(result, Err(SitegenError::InvalidHtml { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn rejects_progress_tasks_that_do_not_link_to_their_plan_heading() {
        let site = build_fixture_site("green").unwrap();
        mutate_index(&site, |html| {
            html.replace(
                "data-progress-task=\"pages-foundation\" href=\"#plan-pages-foundation\"",
                "data-progress-task=\"pages-foundation\" href=\"#play\"",
            )
        });

        let result = validate_site_output(site.root(), &fixture("green/progress.json"));
        assert!(
            matches!(result, Err(SitegenError::InvalidHtml { .. })),
            "{result:?}"
        );
    }

    fn mutate_index(site: &support::GeneratedSite, mutation: impl FnOnce(String) -> String) {
        let path = site.root().join("index.html");
        fs::write(&path, mutation(read(&path))).unwrap();
    }
}

mod build_safety_contract {
    use super::*;
    use midcreek_cs_1::sitegen::{
        trusted_playable_roots_in, validate_output_path_in, validate_output_path_under,
    };
    use std::path::PathBuf;

    #[test]
    fn rejects_source_directories_as_site_output() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"));

        assert_eq!(
            validate_output_path(&repository.join("docs")),
            Err(SitegenError::UnsafeOutputPath {
                path: repository.join("docs"),
            })
        );
    }

    #[test]
    fn rejects_reference_paths_outside_the_approved_manifest() {
        let mut inputs = site_inputs("green");
        inputs.reference_manifest.assets[0].public_path = "../../Cargo.toml".to_owned();
        let output =
            std::env::temp_dir().join(format!("midcreek-unsafe-reference-{}", std::process::id()));

        let result = build_site(&inputs, &output);
        let _ = fs::remove_dir_all(output);

        assert!(matches!(result, Err(SitegenError::Reference(_))));
    }

    /// A relocated `sitegen` still has to protect the repository it was told
    /// about, and still has to accept that repository's own build root. Both
    /// are decided from the declared root, not from the path the binary was
    /// compiled in.
    #[test]
    fn a_declared_repository_protects_its_own_tree_and_allows_its_own_build_root() {
        let declared = relocated_repository("declared-root");

        assert_eq!(
            validate_output_path_in(declared.path(), &declared.path().join("target/site")),
            Ok(())
        );
        assert_eq!(
            validate_output_path_in(declared.path(), &declared.path().join("docs")),
            Err(SitegenError::UnsafeOutputPath {
                path: declared.path().join("docs"),
            }),
            "a declared repository's own source tree is never a publication target"
        );
        assert!(
            trusted_playable_roots_in(declared.path())
                .contains(&fs::canonicalize(declared.path().join("target")).unwrap()),
            "a declared repository's build root is where its packaged game comes from"
        );
    }

    /// The compile-time root keeps protecting the checkout the binary was
    /// built in, so declaring another repository can never open the real
    /// source tree up as an output directory.
    #[test]
    fn a_declared_repository_never_unlocks_the_compiled_repositorys_source_tree() {
        let declared = relocated_repository("declared-root-containment");
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs");

        assert_eq!(
            validate_output_path_in(declared.path(), &source),
            Err(SitegenError::UnsafeOutputPath { path: source })
        );
    }

    /// A relocated binary's compiled-in checkout is not on the machine, and a
    /// caller that declares no repository of its own leaves the guard with
    /// nothing to compare the output against. Skipping the roots that do not
    /// resolve is what lets a relocated run work at all, so the case where
    /// *none* of them resolves is the one that must refuse: a check that can
    /// prove nothing may not report success.
    #[test]
    fn an_output_path_with_no_resolvable_source_tree_is_refused() {
        let declared = relocated_repository("no-resolvable-root");
        let output = declared.path().join("target/site");
        let missing = [
            PathBuf::from("/midcreek-no-such-declared-checkout"),
            PathBuf::from("/midcreek-no-such-compiled-checkout"),
        ];

        assert_eq!(
            validate_output_path_under(&missing, &output),
            Err(SitegenError::UnknownRepository {
                candidates: missing.to_vec(),
            }),
            "a run that can name no source tree must refuse to publish"
        );
        assert_eq!(
            validate_output_path_under(&[declared.path().to_path_buf()], &output),
            Ok(()),
            "one resolvable root is all the same output needs to be accepted"
        );
        assert_eq!(
            validate_output_path_under(
                &[missing[0].clone(), declared.path().to_path_buf()],
                &declared.path().join("docs")
            ),
            Err(SitegenError::UnsafeOutputPath {
                path: declared.path().join("docs"),
            }),
            "an unresolvable root beside a real one must not weaken the real one"
        );
    }

    /// A minimal repository copy: the approved references the generator reads,
    /// its own build root, and a source directory that must stay protected.
    fn relocated_repository(name: &str) -> RelocatedRepository {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/relocated-repositories")
            .join(format!(
                "{name}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        fs::create_dir_all(root.join("docs/reference")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        for relative in [
            "docs/reference/cel-shift-key-art.png",
            "docs/reference/cel-shift-character-sheet.png",
            "docs/reference/manifest.json",
        ] {
            fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR")).join(relative),
                root.join(relative),
            )
            .unwrap();
        }
        RelocatedRepository(root)
    }

    struct RelocatedRepository(std::path::PathBuf);

    impl RelocatedRepository {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for RelocatedRepository {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// `sitegen build` reads the approved references out of the repository its
    /// own inputs declare. A copy whose references were tampered with is
    /// refused even though the compiled-in checkout still holds the approved
    /// ones, which is the only way to prove the declared root is what was
    /// really read.
    #[test]
    fn the_build_cli_reads_the_references_of_the_repository_its_inputs_declare() {
        let declared = relocated_repository("declared-references");
        let workspace = relocated_repository("declared-references-inputs");
        let inputs = workspace.path().join("inputs.json");
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sitegen/green");
        fs::write(
            &inputs,
            serde_json::json!({
                "repository": declared.path(),
                "progress": fixtures.join("progress.json"),
                "plan": fixtures.join("plan.md"),
                "reference_manifest": declared.path().join("docs/reference/manifest.json"),
                "workflow": fixtures.join("workflow.json"),
                "repo": fixtures.join("repo.json"),
            })
            .to_string(),
        )
        .unwrap();
        let output = declared.path().join("target/site");

        let published = Command::new(env!("CARGO_BIN_EXE_sitegen"))
            .current_dir(workspace.path())
            .args([
                "build",
                "--inputs",
                inputs.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
            ])
            .output()
            .expect("sitegen should launch");
        assert_eq!(
            published.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&published.stderr)
        );
        assert!(output.join("reference/cel-shift-key-art.png").is_file());

        fs::write(
            declared.path().join("docs/reference/cel-shift-key-art.png"),
            b"not the approved reference",
        )
        .unwrap();
        fs::remove_dir_all(&output).unwrap();
        let tampered = Command::new(env!("CARGO_BIN_EXE_sitegen"))
            .current_dir(workspace.path())
            .args([
                "build",
                "--inputs",
                inputs.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
            ])
            .output()
            .expect("sitegen should launch");
        let stderr = String::from_utf8_lossy(&tampered.stderr).into_owned();

        assert_eq!(
            tampered.status.code(),
            Some(1),
            "the declared repository's tampered reference must be refused: {stderr}"
        );
        assert!(stderr.contains("SHA-256"), "{stderr}");
    }
}

mod progress_contract {
    use super::*;

    #[test]
    fn accepts_one_dependency_ready_current_task() {
        let document = fixture("green-progress.json");
        assert!(validate_progress(&document, &plan_ids(), &repo_facts()).is_ok());
    }

    mod validate_cli {
        use super::*;

        #[test]
        fn extracts_every_progress_task_from_the_reviewed_plan() {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            let markdown = read(root.join("docs/implementation-plan.md"));

            assert_eq!(
                plan_task_ids_from_markdown(&markdown),
                [
                    "foundation-contracts",
                    "pages-foundation",
                    "autonomous-assets",
                    "data-hall",
                    "technician-movement",
                    "camera-orbit",
                    "operations-loop",
                    "operations-hud",
                    "pages-playable",
                    "autonomous-verification",
                    "pages-verification",
                    "pages-status-always",
                    "ci-baseline",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect()
            );
        }

        /// The reviewed plan declares task IDs through heading prose and
        /// nothing else, so the coupling is total: a heading reworded by one
        /// article stops declaring its IDs, and every progress task naming one
        /// stops validating. Anybody loosening the match to a prefix, a case
        /// fold, or a task number would break that contract silently.
        #[test]
        fn renaming_a_reviewed_plan_heading_drops_the_task_ids_it_declared() {
            let declared = plan_task_ids_from_markdown(
                "## Task 9: Add CI and publish the reproducible POC baseline\n",
            );
            assert_eq!(
                declared,
                ["ci-baseline"].into_iter().map(str::to_owned).collect()
            );

            for reworded in [
                "## Task 9: Add CI and publish a reproducible POC baseline\n",
                "## Task 9: add CI and publish the reproducible POC baseline\n",
                "## Task 9\n",
                "## Task 9: Add CI and publish the reproducible POC baseline (revised)\n",
            ] {
                assert!(
                    plan_task_ids_from_markdown(reworded).is_empty(),
                    "{reworded}"
                );
            }
        }

        /// Heading depth is editorial, so the same reviewed text declares the
        /// same IDs wherever the plan nests it.
        #[test]
        fn a_reviewed_heading_declares_its_task_ids_at_every_depth() {
            for depth in 1..=6 {
                let heading = format!(
                    "{} Task 5: Add clamped four-way camera orbit\n",
                    "#".repeat(depth)
                );
                assert_eq!(
                    plan_task_ids_from_markdown(&heading),
                    ["camera-orbit"].into_iter().map(str::to_owned).collect(),
                    "{heading}"
                );
            }
        }

        #[test]
        fn invalid_arguments_exit_with_code_two() {
            let output = Command::new(env!("CARGO_BIN_EXE_sitegen"))
                .output()
                .expect("sitegen should launch");

            assert_eq!(output.status.code(), Some(2));
        }

        #[test]
        fn invalid_content_exits_with_code_one_and_prints_each_error() {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            let output = Command::new(env!("CARGO_BIN_EXE_sitegen"))
                .current_dir(root)
                .args([
                    "validate",
                    "--progress",
                    "tests/fixtures/sitegen/two-current.json",
                    "--plan",
                    "docs/implementation-plan.md",
                    "--repository",
                    ".",
                ])
                .output()
                .expect("sitegen should launch");

            assert_eq!(output.status.code(), Some(1));
            let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
            assert_eq!(stderr.lines().count(), 2);
            assert!(stderr.contains(
                "task pages-foundation started before dependency foundation-contracts was done"
            ));
            assert!(stderr.contains("multiple tasks are in progress"));
        }

        #[test]
        fn valid_content_prints_the_current_task_and_exits_zero() {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            let output = Command::new(env!("CARGO_BIN_EXE_sitegen"))
                .current_dir(root)
                .args([
                    "validate",
                    "--progress",
                    "docs/progress.json",
                    "--plan",
                    "docs/implementation-plan.md",
                    "--repository",
                    ".",
                ])
                .output()
                .expect("sitegen should launch");

            assert_eq!(output.status.code(), Some(0));
            assert_eq!(String::from_utf8(output.stdout).unwrap(), "ci-baseline\n");
            assert!(output.stderr.is_empty());
        }
    }

    mod publication_inputs {
        use super::*;

        #[test]
        fn canonical_progress_matches_the_approved_ordered_task_graph() {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            let document: midcreek_cs_1::sitegen::ProgressDocument =
                serde_json::from_str(&read(root.join("docs/progress.json")))
                    .expect("canonical progress should match its strict schema");
            let expected = [
                (
                    "foundation-contracts",
                    ProgressStatus::Done,
                    &[][..],
                    "Pinned the Bevy project and established reviewed contracts.",
                    Some("df0da32ab61a6cb6901bfd4b3cdbbcaec75bccc2"),
                ),
                (
                    "pages-foundation",
                    ProgressStatus::Done,
                    &["foundation-contracts"][..],
                    "Published the status, plan, challenge, comparison, test, and commit sections as the status-only Pages hub.",
                    Some("HEAD"),
                ),
                (
                    "autonomous-assets",
                    ProgressStatus::Done,
                    &["pages-foundation"][..],
                    "Generated every cel-shift asset autonomously from declarative RON: an 11-bone rigged technician with Idle, Walk and Repair clips, a merged eight-cabinet rack row with server slots and status lights, a cooling unit, the red utility cart and yellow step stool, and overhead tray plus black hose modules.",
                    Some("HEAD"),
                ),
                (
                    "data-hall",
                    ProgressStatus::Done,
                    &["autonomous-assets"][..],
                    "Loaded every generated GLB through an explicit Loading, Ready, and Failed asset state with no procedural fallback, cached one unit mesh per primitive shape and one unlit material per palette role, and spawned the authored 40 m square hall: a polished floor inside low perimeter walls, four rack rows separated by three traversable aisles, four cooling units, three overhead trays with hose drops, painted yellow aisle and walkway markings, the red service cart, and the yellow step stool. Visual and collider lists stay separate and are joined by stable PropId, the colliders are extracted once into a cached vector, and a room-wide flood fill proves every aisle checkpoint shares one walkable component with the player spawn.",
                    Some("HEAD"),
                ),
                (
                    "technician-movement",
                    ProgressStatus::Done,
                    &["data-hall"][..],
                    "Spawned the generated 11-bone technician only after the assets and the hall were both ready, bound every required rig node into explicit PlayerParts with the rest transform captured before any clip played, and moved it from the real ButtonInput<KeyCode> arrow state. A public ViewBasis resource, initialised to the reviewed NorthEast 45-degree view, is the only screen-to-world interface movement reads, so the orbit task becomes its runtime updater without changing a movement rule. Opposite arrows cancel and diagonals are normalized, world X and world Z resolve separately against the cached colliders and radius-aware room bounds so the technician slides along rack faces, and facing plus the Idle and Walk clips follow the accepted displacement rather than the request. The idle transition stops Walk, explicitly restores every PlayerParts rest transform, and only then plays Idle; the Repair clip is bound for the operations task and nothing plays it yet. Missing, duplicate, and stale rig nodes are typed failures that stop movement instead of being silently skipped.",
                    Some("HEAD"),
                ),
                (
                    "camera-orbit",
                    ProgressStatus::Done,
                    &["technician-movement"][..],
                    "Spawned the one orthographic game camera with the reviewed fixed 26 m by 14.625 m rectangle, 57-degree elevation, zero roll, and the initial NorthEast 45-degree yaw, which is all the authored unlit hall needs to be visible. Real Q and E just_pressed frames retarget the desired heading immediately, opposite keys on one frame cancel exactly, and the yaw eases with smoothstep at a constant 90 degrees per 0.30 seconds, so a retarget mid-tween starts at the interpolated yaw and its duration scales with the shortest remaining angle instead of restarting a fixed clock. CameraPlugin is now the sole runtime updater of ViewBasis and publishes the live interpolated basis in UpdateOrbitIntent, before MovePlayer reads it, so the technician walks along the camera it can actually see mid-orbit. Every frame the ground quadrilateral is cast from the current yaw, its axis-aligned extents are subtracted from the active blueprint's own room.coverage, and the followed technician is clamped into what remains before the transform is derived, so an overridden hall is followed by the coverage it declares rather than by the authored constant. The walkable room stays exactly 40 m, but the camera is clamped against the rendered coverage the new visual apron fills, so it overhangs the room freely and follows every legal player position exactly: every room corner is centred with the full 360 px of margin at every heading and at every exact tween midpoint, while the whole view stays inside the apron and zoom, elevation, and roll stay fixed. Framing is measured against the technician's whole generated spatial envelope rather than its ground origin -- the rest pose and every sampled Idle, Walk, and Repair pose, cel outlines included, 0.7998 m of radius and a 1.9704 m crown -- and every corner of it keeps at least 32 logical pixels of viewport margin, calibrated against an independent 49.23077 px per metre edge point.",
                    Some("HEAD"),
                ),
                (
                    "operations-loop",
                    ProgressStatus::Done,
                    &["data-hall", "technician-movement"][..],
                    "Attached the documented rack state machine to the four authored rack-row HallProp entities by stable PropId, joined to their cached collider rectangles once, and drove it from one seeded ChaCha8 fault stream. The scheduler is a pure sequence generator: every four simulated seconds an opportunity matures and the timer stops accumulating, a full queue pauses it without consuming a single word of randomness, and a drawn candidate whose rack already holds a ticket or is still resolving or cooling down is held rather than rerolled, so the exact rack and severity order is the same whatever the player does and only its timing moves. Tickets carry stable monotonic identifiers and sort Critical before Warning, then by creation tick, then by rack; the queue enforces the maximum of three active tickets and one ticket per rack and reports which rack it refused. A real Space press gathers only open faults, measures distance to each rack's collider rectangle rather than its centre, and selects by severity, then distance, then creation tick, then rack, so an out-of-range press changes nothing and is recorded as a named rejection instead of a silent no-op. Starting a repair in UpdateOperations locks movement in the same frame, before MovePlayer runs, zeroes the published motion, plays the generated Repair clip, and exposes the blue wrench state for the HUD task while the camera keeps orbiting and following. Repair completes on its own after three seconds and releases the lock, the healthy indicator shows for two seconds, the ticket then leaves the queue as the rack begins an eight second cooldown, and only a fully recovered rack is eligible again.",
                    Some("HEAD"),
                ),
                (
                    "operations-hud",
                    ProgressStatus::Done,
                    &["camera-orbit", "operations-loop"][..],
                    "Added src/hud.rs, which draws the whole operations HUD from the live operations model and owns no gameplay state of its own: every frame it reads TicketQueue, RackOperations, RackRoster, MovementLock, LastInteraction, and the real camera, and writes only presentation components plus one observable HudReport, so there is no second ticket model to drift. The top-left stack renders up to three rows straight out of the queue's own global priority order rather than re-sorting it, each row carrying a severity chip, a rack-state chip, a short real label such as T0002 R01 Critical, and a dwell-progress bar; shape carries meaning beside colour, with a square critical chip against a round warning chip and sharp, rounded, and pill badges for fault, repair, and resolved. The status line is derived from live state alone, preferring a running repair, then a real out-of-range rejection while that rejection is still true of the one rack it named, then the queue; move_closer_still_stands re-reads the live queue, that rack's live state, its roster collider, and the technician's live position every frame, so the prompt clears the moment that rack's ticket leaves the queue, that rack stops being Faulted, or the technician walks inside repair range, and another rack's open ticket can never keep a stale prompt alive. Badges are fixed-size screen-space UI nodes anchored every frame from a stable world point 2.4 m above each rack's collider centre through the real Camera::world_to_viewport, reading the camera's own Transform rather than its propagated GlobalTransform so they never lag a tween by a frame, a substitution the camera query enforces with Without<ChildOf> so a parented camera is refused as unusable and its badges are hidden rather than projected through a local transform pretending to be a global one; a visible anchor always gets a fully visible badge because the badge box is clamped inside the viewport and its thin leader line is rotated to end exactly on the projected anchor, while an anchor that leaves the viewport hides explicitly and a refused projection is recorded as a typed error. The bottom-right strip names Arrows, Q, E, and Space and shows which are live, turning the Space cap hard-hat blue and flattening the Arrows cap while a repair holds the technician still. Every colour is a typed PaletteRole, the 216 px queue stack and 40 px control strip keep a 16 px margin and stay outside the central 50% x 50% play rectangle at both 1280x720 and 960x540, and HudReport reports a rack that lost its RackOperations, a ticket whose rack is unknown, a missing row node, a missing badge node exactly once rather than once per write and again per hide, an unusable game camera, and a camera with no viewport, each under its own BadgeVisibility variant so no failure is ever explained with another's name. Shape, glyph, and progress are node-level contracts: the app tests read Node::border_radius off the severity chips and off all three badge states, the glyph text off the real RackBadgeLabel node, and the progress bar's Node::width against the live dwell progress, and each is covered by a mutation that fails when the write is deleted or swapped.",
                    Some("HEAD"),
                ),
                (
                    "pages-playable",
                    ProgressStatus::Done,
                    &["operations-hud"][..],
                    "Packaged the production game for wasm32-unknown-unknown with a pinned wasm-bindgen, added a wasm-only WebReadyPlugin handshake, published it under play/ with last-green metadata, and proved it in headless Chromium.",
                    Some("HEAD"),
                ),
                (
                    "autonomous-verification",
                    ProgressStatus::Done,
                    &["operations-hud"][..],
                    "Build deterministic gameplay and render verification.",
                    Some("HEAD"),
                ),
                (
                    "pages-verification",
                    ProgressStatus::Done,
                    &["pages-playable", "autonomous-verification"][..],
                    "Published the strict public projection of the verification and browser-gate reports, the current frames beside the approved references, the deduplicated screenshot history, and the sanitized gate and metric evidence.",
                    Some("HEAD"),
                ),
                (
                    "pages-status-always",
                    ProgressStatus::Done,
                    &["pages-verification"][..],
                    "Wired the Pages workflow to measure every named gate, upload each job's result and evidence before it concludes, and publish the current commit's status on every push while retaining the last green game, screenshots, and history.",
                    Some("HEAD"),
                ),
                (
                    "ci-baseline",
                    ProgressStatus::InProgress,
                    &["pages-status-always"][..],
                    "Publish and verify the final POC baseline.",
                    None,
                ),
            ];

            assert_eq!(document.schema_version, 1);
            assert_eq!(document.project, "Cell Shift Data Center POC");
            assert_eq!(document.tasks.len(), expected.len());
            for (task, (id, status, dependencies, summary, commit)) in
                document.tasks.iter().zip(expected)
            {
                assert_eq!(task.id, id);
                assert_eq!(task.status, status);
                assert_eq!(task.depends_on, dependencies);
                assert_eq!(task.summary, summary);
                assert_eq!(task.completed_commit.as_deref(), commit);
            }

            let challenges = document
                .challenges
                .iter()
                .map(|challenge| challenge.id.as_str())
                .collect::<Vec<_>>();
            assert_eq!(
                challenges,
                vec![
                    "byte-reproducible-assets-without-a-dcc",
                    "loud-asset-loading-without-a-gpu",
                    "rig-handles-that-survive-instance-respawn",
                    "clamped-orbit-cannot-frame-a-room-corner",
                    "seeded-faults-that-player-timing-cannot-perturb",
                    "screen-space-badges-that-survive-orbit-and-resize",
                    "a-browser-gate-that-cannot-pass-on-a-blank-page",
                    "a-hundred-megabyte-wasm-game-cannot-be-published",
                    "generated-materials-rendered-far-brighter-than-the-authored-palette",
                    "the-overhead-trays-hid-the-technician-at-two-of-the-four-headings",
                    "history-images-arrive-after-the-build-that-links-them",
                    "a-job-that-stops-at-its-first-failure-publishes-nothing",
                    "capture-waits-consume-the-active-work-watchdog",
                    "retained-game-is-not-rendered-on-the-homepage",
                ]
            );
            for challenge in &document.challenges {
                assert!(!challenge.title.trim().is_empty());
                assert!(!challenge.impact.trim().is_empty());
                assert!(!challenge.approach.trim().is_empty());
                // An open challenge still carries its full context, but it has
                // no commit that closed it, because nothing has.
                match challenge.status {
                    ChallengeStatus::Resolved => {
                        assert!(
                            challenge
                                .resolution
                                .as_deref()
                                .is_some_and(|resolution| !resolution.trim().is_empty()),
                            "{} must record what was actually done about it",
                            challenge.id
                        );
                        assert_eq!(challenge.resolved_commit.as_deref(), Some("HEAD"));
                    }
                    ChallengeStatus::Open => {
                        assert_eq!(challenge.resolution, None);
                        assert_eq!(
                            challenge.resolved_commit, None,
                            "{} is not resolved, so it must not claim a resolving commit",
                            challenge.id
                        );
                    }
                }
            }
            assert_eq!(
                document
                    .challenges
                    .iter()
                    .filter(|challenge| challenge.status != ChallengeStatus::Resolved)
                    .map(|challenge| challenge.id.as_str())
                    .collect::<Vec<_>>(),
                vec![
                    "capture-waits-consume-the-active-work-watchdog",
                    "retained-game-is-not-rendered-on-the-homepage",
                ],
                "the canonical progress document must expose the two current baseline blockers"
            );
        }

        #[test]
        fn published_plan_matches_the_approved_master_plan() {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            assert_eq!(
                sha256(root.join("docs/implementation-plan.md")),
                "bc948da3974c990b42f5bdd77ebeb347c9e70d1b37a951c26dac1969a8b475f4"
            );
        }

        #[test]
        fn published_plan_contains_no_absolute_local_paths() {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            let plan = read(root.join("docs/implementation-plan.md"));
            let has_windows_drive_root = plan.as_bytes().windows(3).any(|window| {
                window[0].is_ascii_alphabetic()
                    && window[1] == b':'
                    && matches!(window[2], b'/' | b'\\')
            });

            assert!(!plan.contains("/Users/"), "plan contains a macOS user path");
            assert!(!plan.contains("file://"), "plan contains a file URL");
            assert!(
                !has_windows_drive_root,
                "plan contains a Windows drive-root path"
            );
        }

        #[test]
        fn reference_manifest_matches_the_approved_pngs() {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            let manifest: ReferenceManifest =
                serde_json::from_str(&read(root.join("docs/reference/manifest.json")))
                    .expect("reference manifest should match its strict schema");
            let expected = [
                (
                    "Cel Shift key art",
                    "../midcreek-concept/themes/cel-shift/masters/key-art/04-diamond-bright.png",
                    "docs/reference/cel-shift-key-art.png",
                    "a30e12b63a36743015b1c73eeca6248a8b8ee974cf007f23666dc101f06c0e75",
                ),
                (
                    "Cel Shift character sheet",
                    "../midcreek-concept/themes/cel-shift/masters/animation/01-model-sheet.png",
                    "docs/reference/cel-shift-character-sheet.png",
                    "8a5a31e7bceb8ad16b3481d2bae89e7a32bb4edd0ef711b7d07a26f177cf6b25",
                ),
            ];

            assert_eq!(manifest.assets.len(), expected.len());
            for (asset, (name, source_path, public_path, expected_hash)) in
                manifest.assets.iter().zip(expected)
            {
                assert_eq!(asset.name, name);
                assert_eq!(asset.source_path, source_path);
                assert_eq!(asset.public_path, public_path);
                assert_eq!((asset.width, asset.height), (1536, 1024));
                assert_eq!(asset.sha256, expected_hash);
                assert_eq!(sha256(root.join(public_path)), expected_hash);
            }
        }

        #[test]
        fn reference_manifest_rejects_unknown_fields() {
            let json = r#"{
                "assets": [{
                    "name": "Cel Shift key art",
                    "source_path": "source.png",
                    "public_path": "public.png",
                    "sha256": "abc",
                    "width": 1,
                    "height": 1,
                    "extra": true
                }]
            }"#;

            assert!(serde_json::from_str::<ReferenceManifest>(json).is_err());
        }

        #[test]
        fn reference_validation_rejects_manifest_drift() {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            let mut manifest: ReferenceManifest =
                serde_json::from_str(&read(root.join("docs/reference/manifest.json"))).unwrap();
            manifest.assets[0].sha256 =
                "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff".into();

            let errors = validate_reference_manifest(&manifest, root).unwrap_err();

            assert!(errors.iter().any(|error| matches!(
                error,
                ReferenceError::ManifestFieldMismatch { asset, field, .. }
                    if asset == "Cel Shift key art" && field == "sha256"
            )));
        }

        #[test]
        fn reference_validation_rejects_changed_destination_bytes() {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            let manifest: ReferenceManifest =
                serde_json::from_str(&read(root.join("docs/reference/manifest.json"))).unwrap();
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let temporary_root = std::env::temp_dir()
                .join(format!("midcreek-sitegen-{unique}-{}", std::process::id()));
            let reference_dir = temporary_root.join("docs/reference");
            fs::create_dir_all(&reference_dir).unwrap();
            for asset in &manifest.assets {
                fs::copy(
                    root.join(&asset.public_path),
                    temporary_root.join(&asset.public_path),
                )
                .unwrap();
            }
            fs::write(
                temporary_root.join("docs/reference/cel-shift-key-art.png"),
                b"changed",
            )
            .unwrap();

            let errors = validate_reference_manifest(&manifest, &temporary_root).unwrap_err();
            fs::remove_dir_all(&temporary_root).unwrap();

            assert!(errors.iter().any(|error| matches!(
                error,
                ReferenceError::AssetHashMismatch { path, .. }
                    if path == Path::new("docs/reference/cel-shift-key-art.png")
            )));
        }
    }

    #[test]
    fn rejects_two_current_tasks() {
        let errors = validate_fixture("two-current.json");
        assert!(errors.contains(&ProgressError::MultipleCurrentTasks));
    }

    #[test]
    fn rejects_done_task_without_commit() {
        let errors = validate_fixture("done-without-commit.json");
        assert!(errors.iter().any(|error| matches!(
            error,
            ProgressError::MissingCompletionCommit { task_id }
                if task_id == "pages-foundation"
        )));
    }

    #[test]
    fn resolves_head_to_the_workflow_commit() {
        let facts = repo_facts();
        let resolved = resolve_commit_ref("HEAD", &facts).unwrap();
        assert_eq!(resolved, facts.head_sha);
    }

    #[test]
    fn rejects_unknown_fields_in_every_progress_source_type() {
        let document_with_extra = r#"{
            "schema_version": 1,
            "project": "Cell Shift Data Center POC",
            "tasks": [],
            "challenges": [],
            "extra": true
        }"#;
        let task_with_extra = r#"{
            "schema_version": 1,
            "project": "Cell Shift Data Center POC",
            "tasks": [{
                "id": "foundation-contracts",
                "title": "Establish reviewed contracts",
                "status": "done",
                "depends_on": [],
                "summary": "Done.",
                "completed_commit": "1111111111111111111111111111111111111111",
                "extra": true
            }],
            "challenges": []
        }"#;
        let challenge_with_extra = r#"{
            "schema_version": 1,
            "project": "Cell Shift Data Center POC",
            "tasks": [],
            "challenges": [{
                "id": "render-variance",
                "title": "Render variance",
                "status": "open",
                "impact": "Hashes vary.",
                "approach": "Use bounded metrics.",
                "resolution": null,
                "resolved_commit": null,
                "extra": true
            }]
        }"#;

        for json in [document_with_extra, task_with_extra, challenge_with_extra] {
            assert!(
                serde_json::from_str::<midcreek_cs_1::sitegen::ProgressDocument>(json).is_err()
            );
        }
    }

    #[test]
    fn reports_task_errors_in_document_order() {
        let mut document = fixture("green-progress.json");
        document.tasks[0].completed_commit = None;
        document.tasks[1].status = ProgressStatus::Future;
        document.tasks[1].completed_commit =
            Some("2222222222222222222222222222222222222222".to_owned());

        let errors = validate_progress(&document, &plan_ids(), &repo_facts()).unwrap_err();
        let missing_index = errors
            .iter()
            .position(|error| {
                matches!(
                    error,
                    ProgressError::MissingCompletionCommit { task_id }
                        if task_id == "foundation-contracts"
                )
            })
            .unwrap();
        let unexpected_index = errors
            .iter()
            .position(|error| {
                matches!(
                    error,
                    ProgressError::UnexpectedCompletionCommit { task_id }
                        if task_id == "pages-foundation"
                )
            })
            .unwrap();

        assert!(missing_index < unexpected_index);
    }

    #[test]
    fn aggregates_missing_challenge_context_and_resolution() {
        let mut document = fixture("green-progress.json");
        document.challenges = vec![
            Challenge {
                id: "render-variance".to_owned(),
                title: "Render variance".to_owned(),
                status: ChallengeStatus::Open,
                impact: " ".to_owned(),
                approach: String::new(),
                resolution: None,
                resolved_commit: None,
            },
            Challenge {
                id: "browser-readiness".to_owned(),
                title: "Browser readiness".to_owned(),
                status: ChallengeStatus::Resolved,
                impact: "The game might not render.".to_owned(),
                approach: "Wait for the readiness signal.".to_owned(),
                resolution: Some(" ".to_owned()),
                resolved_commit: None,
            },
        ];

        assert_eq!(
            validate_progress(&document, &plan_ids(), &repo_facts()).unwrap_err(),
            [
                ProgressError::MissingChallengeContext {
                    challenge_id: "render-variance".to_owned(),
                    field: "impact".to_owned(),
                },
                ProgressError::MissingChallengeContext {
                    challenge_id: "render-variance".to_owned(),
                    field: "approach".to_owned(),
                },
                ProgressError::MissingChallengeResolution {
                    challenge_id: "browser-readiness".to_owned(),
                },
                ProgressError::MissingChallengeContext {
                    challenge_id: "browser-readiness".to_owned(),
                    field: "resolved_commit".to_owned(),
                },
            ]
        );
    }

    /// A challenge that names a commit nobody can find is a different mistake
    /// from a challenge that names none: the first is a wrong reference to
    /// chase, the second is a missing field to fill in. Reporting the first as
    /// the second sends the reader looking for an empty field that is right
    /// there in the document.
    #[test]
    fn a_resolved_challenge_naming_an_unknown_commit_is_reported_as_unknown_not_missing() {
        let mut document = fixture("green-progress.json");
        document.challenges = vec![Challenge {
            id: "browser-readiness".to_owned(),
            title: "Browser readiness".to_owned(),
            status: ChallengeStatus::Resolved,
            impact: "The game might not render.".to_owned(),
            approach: "Wait for the readiness signal.".to_owned(),
            resolution: Some("Waited for the readiness signal.".to_owned()),
            resolved_commit: Some("3333333333333333333333333333333333333333".to_owned()),
        }];

        let errors = validate_progress(&document, &plan_ids(), &repo_facts()).unwrap_err();

        assert_eq!(
            errors,
            [ProgressError::UnknownChallengeCommit {
                challenge_id: "browser-readiness".to_owned(),
                commit: "3333333333333333333333333333333333333333".to_owned(),
            }]
        );
        assert_eq!(
            errors[0].to_string(),
            "challenge browser-readiness references unknown commit \
             3333333333333333333333333333333333333333"
        );
    }

    /// The commit a resolved challenge really names is accepted, so the new
    /// error names a wrong reference and nothing else.
    #[test]
    fn a_resolved_challenge_naming_a_known_commit_is_accepted() {
        let mut document = fixture("green-progress.json");
        document.challenges = vec![Challenge {
            id: "browser-readiness".to_owned(),
            title: "Browser readiness".to_owned(),
            status: ChallengeStatus::Resolved,
            impact: "The game might not render.".to_owned(),
            approach: "Wait for the readiness signal.".to_owned(),
            resolution: Some("Waited for the readiness signal.".to_owned()),
            resolved_commit: Some("2222222222222222222222222222222222222222".to_owned()),
        }];

        assert_eq!(
            validate_progress(&document, &plan_ids(), &repo_facts()),
            Ok(())
        );
    }
}

mod playable_publication_contract {
    use super::*;
    use midcreek_cs_1::sitegen::{PlayableBuild, resolve_playable_package, trusted_playable_roots};
    use std::path::PathBuf;

    const GREEN_COMMIT: &str = "1111111111111111111111111111111111111111";

    #[test]
    fn a_green_playable_build_is_copied_under_play_and_embedded_in_the_hub() {
        let package = package("complete", PACKAGE_FILES);
        let mut inputs = site_inputs("green");
        inputs.playable = Some(playable(package.path()));

        let site = build_site_from_inputs("playable-green", &inputs).unwrap();
        let html = site.index_html();

        assert!(site.root().join("play/index.html").is_file());
        assert!(site.root().join("play/game_bg.wasm").is_file());
        assert!(site.root().join("play/assets/generated/rack.glb").is_file());
        assert!(html.contains("play/index.html"), "{html}");
        assert!(!html.contains("No verified playable build yet"));
        assert_eq!(
            site.manifest().playable_commit.as_deref(),
            Some(GREEN_COMMIT)
        );
    }

    #[test]
    fn a_green_playable_build_records_last_green_metadata() {
        let package = package("metadata", PACKAGE_FILES);
        let mut inputs = site_inputs("green");
        inputs.playable = Some(playable(package.path()));

        let site = build_site_from_inputs("playable-metadata", &inputs).unwrap();
        let metadata: midcreek_cs_1::sitegen::LastGreenManifest =
            serde_json::from_str(&read(site.root().join("last-green.json"))).unwrap();

        assert_eq!(metadata.source_commit, GREEN_COMMIT);
        assert_eq!(metadata.semantic_visual_hash, None);
        assert_eq!(
            metadata.game_files,
            [
                "play/assets/generated/rack.glb",
                "play/game.js",
                "play/game_bg.wasm",
                "play/index.html",
                "play/play.css",
                "play/play.js",
            ]
            .map(PathBuf::from)
        );
        assert!(metadata.screenshot_files.is_empty());
    }

    #[test]
    fn a_package_without_the_wasm_payload_is_refused_instead_of_published() {
        let package = package(
            "no-wasm",
            &[
                ("index.html", "<!doctype html>"),
                ("play.js", "// bootstrap"),
                ("play.css", "/* shell */"),
                ("game.js", ""),
                ("assets/generated/rack.glb", "glTF"),
            ],
        );
        let mut inputs = site_inputs("green");
        inputs.playable = Some(playable(package.path()));

        let result = build_site_from_inputs("playable-broken", &inputs);

        assert!(
            matches!(
                &result,
                Err(SitegenError::MissingInput { path })
                    if path.file_name().is_some_and(|name| name == "game_bg.wasm")
            ),
            "{:?}",
            result.as_ref().err()
        );
    }

    #[test]
    fn a_package_without_generated_assets_is_refused_instead_of_published() {
        let package = package(
            "no-assets",
            &[
                ("index.html", "<!doctype html>"),
                ("play.js", "// bootstrap"),
                ("play.css", "/* shell */"),
                ("game.js", ""),
                ("game_bg.wasm", "\0asm"),
            ],
        );
        let mut inputs = site_inputs("green");
        inputs.playable = Some(playable(package.path()));

        let result = build_site_from_inputs("playable-no-assets", &inputs);

        assert!(
            matches!(
                &result,
                Err(SitegenError::MissingInput { path })
                    if path.file_name().is_some_and(|name| name == "assets")
            ),
            "{:?}",
            result.as_ref().err()
        );
    }

    #[test]
    fn a_complete_package_that_escapes_every_trusted_root_is_refused() {
        // Every required file is present, so the only reason this can fail is
        // that the directory is not inside a trusted build root.
        let package = untrusted_package("escaping", PACKAGE_FILES);
        for (relative, _) in PACKAGE_FILES {
            assert!(package.path().join(relative).is_file(), "{relative}");
        }
        let roots = trusted_playable_roots();
        assert!(
            !roots
                .iter()
                .any(|root| package.path().starts_with(root.as_path())),
            "the escape fixture must sit outside {roots:?}"
        );
        let mut inputs = site_inputs("green");
        inputs.playable = Some(playable(package.path()));

        let result = build_site_from_inputs("playable-escaping", &inputs);

        assert!(
            matches!(
                &result,
                Err(SitegenError::UntrustedPlayablePackage { path })
                    if path == package.path()
            ),
            "{:?}",
            result.map(|site| site.index_html()).err()
        );
    }

    #[test]
    fn a_package_under_the_repository_target_root_is_trusted() {
        let package = package("trusted-target", PACKAGE_FILES);

        let resolved = resolve_playable_package(package.path(), &trusted_playable_roots()).unwrap();

        assert_eq!(resolved, fs::canonicalize(package.path()).unwrap());
    }

    #[test]
    fn a_package_under_the_runner_temp_root_is_trusted() {
        let runner_temp = untrusted_package("runner-temp-root", &[]);
        let directory = runner_temp.path().join("web");
        write_package(&directory, PACKAGE_FILES);
        let roots = vec![fs::canonicalize(runner_temp.path()).unwrap()];

        let resolved = resolve_playable_package(&directory, &roots).unwrap();

        assert_eq!(resolved, fs::canonicalize(&directory).unwrap());
    }

    #[test]
    fn a_relative_parent_escape_from_a_trusted_root_is_refused() {
        let package = package("relative-escape", PACKAGE_FILES);
        let escape = package.path().join("../../..");

        let result = resolve_playable_package(&escape, &trusted_playable_roots());

        assert!(
            matches!(&result, Err(SitegenError::UntrustedPlayablePackage { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn a_package_in_the_repository_source_tree_is_refused() {
        let source_tree = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

        let result = resolve_playable_package(&source_tree, &trusted_playable_roots());

        assert!(
            matches!(&result, Err(SitegenError::UntrustedPlayablePackage { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn a_symlink_inside_a_trusted_root_that_points_outside_it_is_refused() {
        let outside = untrusted_package("symlink-escape-target", PACKAGE_FILES);
        let holder = package("symlink-escape-holder", &[]);
        let link = holder.path().join("web");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        assert!(link.join("game_bg.wasm").is_file());

        let result = resolve_playable_package(&link, &trusted_playable_roots());

        assert!(
            matches!(&result, Err(SitegenError::UntrustedPlayablePackage { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn no_trusted_root_is_itself_a_publishable_package() {
        let roots = trusted_playable_roots();

        assert!(
            !roots.is_empty(),
            "the repository build root is always trusted, so an empty list is a bug \
             in root discovery rather than a fact about this machine"
        );
        for root in &roots {
            let result = resolve_playable_package(root, &roots);
            assert!(
                matches!(&result, Err(SitegenError::UntrustedPlayablePackage { .. })),
                "{}: {result:?}",
                root.display()
            );
        }
    }

    const PACKAGE_FILES: &[(&str, &str)] = &[
        ("index.html", "<!doctype html><html><body></body></html>"),
        ("game.js", "export default function init() {}"),
        ("game_bg.wasm", "\0asm"),
        ("play.js", "// bootstrap"),
        ("play.css", "/* shell */"),
        ("assets/generated/rack.glb", "glTF"),
    ];

    fn playable(directory: &Path) -> PlayableBuild {
        PlayableBuild {
            directory: directory.to_path_buf(),
            source_commit: GREEN_COMMIT.to_owned(),
            run_url: "https://example.invalid/run/1".to_owned(),
        }
    }

    struct Package(PathBuf);

    impl Package {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Package {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn unique_name(name: &str) -> String {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!(
            "midcreek-web-package-{}-{unique}-{name}",
            std::process::id()
        )
    }

    fn write_package(root: &Path, files: &[(&str, &str)]) {
        fs::create_dir_all(root).unwrap();
        for (relative, contents) in files {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
    }

    /// A package inside the repository build root, where a real package lives.
    fn package(name: &str, files: &[(&str, &str)]) -> Package {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/web-packages")
            .join(unique_name(name));
        write_package(&root, files);
        Package(root)
    }

    /// A package outside every trusted build root.
    ///
    /// The system temporary directory is only outside them on a machine where
    /// `RUNNER_TEMP` is somewhere else, which is true of a GitHub runner and
    /// of a developer machine but is not guaranteed by anything. A run where
    /// that stops holding must say so, rather than let a fixture that is
    /// silently trusted decide what "untrusted" proved.
    fn untrusted_package(name: &str, files: &[(&str, &str)]) -> Package {
        let root = std::env::temp_dir().join(unique_name(name));
        write_package(&root, files);
        let canonical = fs::canonicalize(&root).unwrap();
        let roots = trusted_playable_roots();
        assert!(
            !roots.iter().any(|trusted| canonical.starts_with(trusted)),
            "{} must sit outside every trusted build root {roots:?}; RUNNER_TEMP is {:?}",
            canonical.display(),
            std::env::var_os("RUNNER_TEMP")
        );
        Package(root)
    }
}

mod web_source_contract {
    use super::*;

    fn repository() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    }

    fn source(relative: &str) -> String {
        read(repository().join(relative))
    }

    #[test]
    fn the_play_template_declares_explicit_browser_states() {
        let html = source("site/templates/play.html");

        assert!(html.contains(r#"data-game-state="loading""#), "{html}");
        assert!(html.contains(r#"id="browser-errors""#), "{html}");
        assert!(html.contains(r#"id="game-canvas""#), "{html}");
    }

    #[test]
    fn the_bootstrap_captures_errors_and_unhandled_rejections() {
        let js = source("site/static/play.js");

        assert!(js.contains(r#"window.addEventListener("error""#), "{js}");
        assert!(
            js.contains(r#"window.addEventListener("unhandledrejection""#),
            "{js}"
        );
        assert!(js.contains("browser-errors"), "{js}");
    }

    #[test]
    fn the_bootstrap_prevents_scrolling_only_for_the_reviewed_keys_while_focused() {
        let js = source("site/static/play.js");

        for key in [
            "ArrowUp",
            "ArrowDown",
            "ArrowLeft",
            "ArrowRight",
            "KeyQ",
            "KeyE",
            "Space",
        ] {
            assert!(js.contains(key), "the bootstrap must scope {key}");
        }
        assert!(js.contains("preventDefault"), "{js}");
        assert!(js.contains("document.activeElement"), "{js}");
    }

    #[test]
    fn the_play_page_carries_a_neutral_scroll_probe_outside_the_canvas() {
        let html = source("site/templates/play.html");

        let probe = html
            .find("data-scroll-probe")
            .expect("the play page must carry a [data-scroll-probe] focus target");
        let canvas_end = html
            .find("</canvas>")
            .expect("the play page must close the canvas element");
        let stage_end = canvas_end
            + html[canvas_end..]
                .find("</div>")
                .expect("the canvas stage must close");
        assert!(
            probe > stage_end,
            "the scroll probe must live outside the canvas stage so focusing it \
             really proves the page scrolls with the canvas unfocused"
        );

        let opened = html[..probe]
            .rfind('<')
            .expect("the scroll probe must sit inside an element");
        let element_end = opened
            + html[opened..]
                .find('>')
                .expect("the scroll probe element must close its start tag");
        let element = &html[opened..element_end];
        let tag: String = element[1..]
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect();
        assert_eq!(
            tag, "div",
            "the probe must be neutral: a button, link, or input would swallow \
             Space itself and the positive control could never prove it scrolls"
        );
        assert!(
            element.contains(r#"tabindex="0""#),
            "the scroll probe must be focusable, found {element}"
        );
        assert!(
            element.contains(r#"id="scroll-probe""#),
            "the scroll probe must be nameable in diagnostics, found {element}"
        );
    }

    #[test]
    fn the_play_page_reserves_more_scroll_room_than_the_browser_gate_demands() {
        let html = source("site/templates/play.html");
        let css = source("site/static/play.css");

        assert!(
            html.contains("data-scroll-reserve"),
            "the play page must mark its deliberate scroll reserve"
        );

        let rule = css_rule(&css, ".scroll-reserve");
        assert!(
            rule.contains("min-height"),
            "the reserve must be a floor, not a fixed height, found {rule}"
        );
        assert!(
            rule.contains("100vh"),
            "the reserve must outgrow whatever viewport the runner has, found {rule}"
        );

        let absolute = reserve_pixels(rule).unwrap_or_else(|error| panic!("{error}"));
        let demanded = gate_constant("MINIMUM_SCROLL_RESERVE_PIXELS");
        assert!(
            absolute >= demanded,
            "the page reserves {absolute}px above a full viewport but the browser \
             gate demands at least {demanded}px of scroll room"
        );
    }

    /// The reserve exists for the browser gate's positive control, which
    /// always loads this page as the whole document. The hub embeds the same
    /// page in a 16:9 iframe, where the reserve only makes the embedded
    /// document a full viewport taller than the frame showing it. A framed
    /// document therefore drops the reserve, and the standalone page — the one
    /// the gate actually measures — keeps every pixel of it.
    #[test]
    fn a_framed_play_page_drops_the_reserve_the_standalone_page_keeps() {
        let css = source("site/static/play.css");
        let embedded = css_rule(&css, r#"body[data-embedded="true"] .scroll-reserve"#);

        assert_eq!(
            reserve_pixels(embedded),
            Ok(0.0),
            "an embedded document must reserve no scroll room at all: {embedded}"
        );
        assert!(
            reserve_pixels(css_rule(&css, ".scroll-reserve"))
                .is_ok_and(|pixels| pixels >= gate_constant("MINIMUM_SCROLL_RESERVE_PIXELS")),
            "the standalone reserve must survive the embedded override"
        );

        for (mode, expected) in [("framed", "true"), ("standalone", "")] {
            let harness = r#"
const fs = require("fs");
const body = { dataset: {} };
global.MutationObserver = class { observe() {} };
global.document = {
  body,
  activeElement: null,
  getElementById: () => null,
  querySelector: () => null,
};
const self = {};
global.window = {
  self,
  top: process.argv[2] === "framed" ? {} : self,
  location: { origin: "https://example.invalid" },
  addEventListener: () => {},
};
try {
  eval(fs.readFileSync(process.argv[1], "utf8"));
} catch (failure) {
  if (!String(failure).includes("game.js")) {
    console.error(failure);
    process.exit(20);
  }
}
const marker = body.dataset.embedded ?? "";
if (marker !== process.argv[3]) {
  console.error(`expected ${process.argv[3] || "no marker"}, got ${marker || "no marker"}`);
  process.exit(21);
}
process.exit(0);
"#;
            let run = Command::new("node")
                .args([
                    "-e",
                    harness,
                    repository().join("site/static/play.js").to_str().unwrap(),
                    mode,
                    expected,
                ])
                .output()
                .expect("Node should execute the dependency-free play bootstrap");

            assert_eq!(
                run.status.code(),
                Some(0),
                "{mode}: stdout {} stderr {}",
                String::from_utf8_lossy(&run.stdout),
                String::from_utf8_lossy(&run.stderr)
            );
        }
    }

    /// The hub embeds the play page, so the two have to agree about what
    /// "embedded" means. The stylesheet names an attribute and the bootstrap
    /// sets one; if they drift apart the reserve silently comes back.
    #[test]
    fn the_stylesheet_and_the_bootstrap_agree_on_the_embedded_marker() {
        let css = source("site/static/play.css");
        let js = source("site/static/play.js");
        let selector = css
            .lines()
            .find(|line| line.contains("data-embedded"))
            .expect("play.css must scope a rule to the embedded document");

        assert!(
            selector.contains(r#"body[data-embedded="true"]"#),
            "{selector}"
        );
        assert!(
            js.contains("dataset.embedded"),
            "the bootstrap must set the attribute the stylesheet keys on: {js}"
        );
        assert!(
            js.contains("window.top"),
            "the bootstrap must decide from the frame it is in: {js}"
        );
    }

    /// One CSS rule body, from a selector that starts its own line.
    fn css_rule<'css>(css: &'css str, selector: &str) -> &'css str {
        let marker = format!("\n{selector} {{");
        let start = css
            .find(&marker)
            .unwrap_or_else(|| panic!("play.css must declare a {selector} rule"))
            + marker.len();
        let body = &css[start..];
        &body[..body.find('}').expect("the rule must close")]
    }

    /// The absolute pixel term of one `min-height` declaration.
    ///
    /// The browser gate demands its scroll reserve in pixels, so a rule stated
    /// in any other absolute unit cannot be compared with that demand at all.
    /// Reporting which unit was found leaves an actionable failure rather than
    /// a number read out of the wrong term.
    fn reserve_pixels(rule: &str) -> Result<f64, String> {
        let declaration = rule
            .split(';')
            .find_map(|declaration| declaration.split_once("min-height:"))
            .map(|(_, value)| value.trim())
            .ok_or_else(|| format!("no min-height declaration in {rule}"))?;
        let terms = match declaration.split_once("calc(") {
            Some((_, rest)) => {
                rest.split_once(')')
                    .ok_or_else(|| format!("unterminated calc() in {declaration}"))?
                    .0
            }
            None => declaration,
        };

        let mut pixels = 0.0;
        for term in terms
            .split('+')
            .map(str::trim)
            .filter(|term| !term.is_empty())
        {
            let digits = term.trim_end_matches(|value: char| value.is_ascii_alphabetic());
            let unit = &term[digits.len()..];
            let value = digits
                .parse::<f64>()
                .map_err(|_| format!("{term:?} is not a length"))?;
            match unit {
                "px" => pixels += value,
                // The viewport term is deliberate: the reserve is stated on top
                // of whatever window the runner has.
                "vh" => {}
                "" if value == 0.0 => {}
                other => {
                    return Err(format!(
                        "the reserve is stated in {other}, which cannot be compared with \
                         the browser gate's pixel demand: {term:?}"
                    ));
                }
            }
        }
        Ok(pixels)
    }

    /// The parser decides whether the reserve satisfies the gate, so a unit it
    /// cannot convert has to be reported rather than silently read as the
    /// number in front of it.
    #[test]
    fn the_reserve_parser_reports_a_unit_it_cannot_compare() {
        assert_eq!(
            reserve_pixels("min-height: calc(100vh + 720px);"),
            Ok(720.0)
        );
        assert_eq!(reserve_pixels("min-height: 0;"), Ok(0.0));

        let rem = reserve_pixels("min-height: calc(100vh + 45rem);\n  border: 3px solid;")
            .expect_err("a rem reserve cannot be compared with a pixel demand");
        assert!(rem.contains("rem"), "{rem}");
        assert!(
            !rem.contains("3px"),
            "the border must never be read as the reserve: {rem}"
        );
        assert!(reserve_pixels("padding: 1.25rem;").is_err());
    }

    /// A numeric constant declared at the top of the browser gate.
    fn gate_constant(name: &str) -> f64 {
        let gate = source("scripts/browser_gate.py");
        let line = gate
            .lines()
            .find(|line| line.starts_with(&format!("{name} = ")))
            .unwrap_or_else(|| panic!("browser_gate.py must declare {name}"));
        line.rsplit(" = ")
            .next()
            .and_then(|value| value.trim().parse::<f64>().ok())
            .unwrap_or_else(|| panic!("{name} must be a number, found {line}"))
    }

    /// The generator judges a published browser report against bounds of its
    /// own. Those bounds are only honest if they are the gate's: a stricter
    /// readiness limit fails a browser the gate itself let through, a stricter
    /// palette floor fails a canvas it accepted, and either verdict marks the
    /// whole run unsuccessful — which suppresses the fourteen native frames
    /// beside it, none of which the browser had anything to do with.
    #[test]
    fn the_published_browser_bounds_are_the_browser_gates_own_constants() {
        assert_eq!(
            midcreek_cs_1::sitegen::BROWSER_READY_LIMIT_SECONDS,
            gate_constant("READY_TIMEOUT_SECONDS"),
            "the published readiness bound must be the gate's own timeout"
        );
        assert_eq!(
            midcreek_cs_1::sitegen::MINIMUM_PALETTE_CLASSES as f64,
            gate_constant("MINIMUM_PALETTE_CLASSES"),
            "the published palette floor must be the gate's own minimum"
        );
    }

    #[test]
    fn the_browser_gate_proves_keyboard_ownership_against_the_neutral_probe() {
        let gate = source("scripts/browser_gate.py");

        for fragment in [
            // trusted events, not synthetic ones
            r#""type": "rawKeyDown""#,
            r#""type": "char""#,
            r#""type": "keyUp""#,
            // the probe must own focus before the positive control runs
            "data-scroll-probe",
            "did not take keyboard focus",
            "inside the canvas",
            // an explicit reserve, not whatever the viewport left over
            "MINIMUM_SCROLL_RESERVE_PIXELS",
            "scrollable pixels, fewer than",
            // both phases through one helper, with per-key evidence
            "def press_control_keys",
            "per-key scroll deltas",
            "POSITIVE_CONTROL_KEYS",
            // frames, not a wall clock: an animated scroll advances per frame
            "NEXT_FRAMES_JS",
            "requestAnimationFrame",
        ] {
            assert!(
                gate.contains(fragment),
                "browser_gate.py must contain {fragment}"
            );
        }

        let body = gate
            .split_once("def check_control_keys_do_not_scroll(")
            .expect("the gate must keep the keyboard ownership check")
            .1;
        assert_eq!(
            body.matches("press_control_keys(session)").count(),
            2,
            "the probed page and the focused canvas must be sent the same sequence"
        );
        assert!(
            !body.contains("Input.dispatchKeyEvent"),
            "the two phases must not grow their own key dispatch"
        );
        assert!(
            body.contains(r#"describe_scroll(session, unfocused)"#)
                && body.contains(r#"describe_scroll(session, focused)"#),
            "both phases must report their per-key deltas on failure"
        );
    }

    #[test]
    fn the_browser_shell_only_uses_relative_urls() {
        for relative in ["site/templates/play.html", "site/static/play.js"] {
            let text = source(relative);
            for absolute in [r#"src="/"#, r#"href="/"#, r#"from "/"#, r#"import("/"#] {
                assert!(
                    !text.contains(absolute),
                    "{relative} must work below a project path prefix, found {absolute}"
                );
            }
        }
    }

    #[test]
    fn the_game_wires_the_browser_handshake_only_for_wasm() {
        let lib = source("src/lib.rs");

        assert!(lib.contains(r#"#[cfg(target_arch = "wasm32")]"#), "{lib}");
        assert!(lib.contains("web::WebReadyPlugin"), "{lib}");
    }

    #[test]
    fn the_web_build_pins_the_locked_wasm_bindgen_version() {
        let locked = locked_version("wasm-bindgen");
        let manifest = source("Cargo.toml");
        let script = source("scripts/build-web.sh");

        assert!(
            manifest.contains(&format!(r#"wasm-bindgen = "={locked}""#)),
            "Cargo.toml must pin wasm-bindgen to the locked {locked}"
        );
        assert!(script.contains("Cargo.lock"), "{script}");
        assert!(script.contains("wasm-bindgen --version"), "{script}");
    }

    #[test]
    fn the_web_build_runs_the_reviewed_packaging_pipeline() {
        let script = source("scripts/build-web.sh");

        for fragment in [
            "set -euo pipefail",
            "--release",
            "wasm32-unknown-unknown",
            "--target web",
            "--no-typescript",
            "assets/generated",
            "site/templates/play.html",
            "site/static/play.js",
        ] {
            assert!(
                script.contains(fragment),
                "build-web.sh must contain {fragment}"
            );
        }
    }

    #[test]
    fn the_browser_gate_enforces_the_reviewed_checks() {
        let script = source("scripts/web-smoke.sh");
        let driver = source("scripts/browser_gate.py");

        assert!(script.contains("set -euo pipefail"), "{script}");
        assert!(script.contains("trap "), "cleanup must be trapped");
        assert!(script.contains("--remote-debugging-port"), "{script}");
        assert!(
            script.contains("--disable-smooth-scrolling"),
            "an animated keyboard scroll cannot be measured deterministically"
        );
        for fragment in [
            "READY_TIMEOUT_SECONDS = 30",
            "data-game-state",
            "browser-errors",
            "Input.dispatchKeyEvent",
            "Page.captureScreenshot",
            "scrollY",
            "rawKeyDown",
            "data-scroll-probe",
            "MINIMUM_SCROLL_RESERVE_PIXELS",
            "MAX_MESSAGE_BYTES",
        ] {
            assert!(
                driver.contains(fragment),
                "browser_gate.py must contain {fragment}"
            );
        }
    }

    #[test]
    fn the_browser_gate_reassembles_fragmented_websocket_messages() {
        let driver = source("scripts/browser_gate.py");

        for fragment in [
            "CONTINUATION = 0x0",
            "CONTROL_OPCODES",
            "MAX_CONTROL_PAYLOAD_BYTES",
            "max_message_bytes",
            "def _handle_control",
            "continuation frame with no message to",
            "stopped answering",
        ] {
            assert!(
                driver.contains(fragment),
                "browser_gate.py must contain {fragment}"
            );
        }

        let run = Command::new("python3")
            .arg(repository().join("scripts/browser_gate_test.py"))
            .output()
            .expect("python3 should run the browser gate unit tests");

        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
    }

    #[test]
    fn the_browser_gate_only_cleans_diagnostics_it_owns() {
        let script = source("scripts/web-smoke.sh");

        for fragment in [
            "midcreek-web-smoke",
            "this script did not create it",
            "refusing to write diagnostics outside",
            "os.path.realpath",
        ] {
            assert!(
                script.contains(fragment),
                "web-smoke.sh must contain {fragment}"
            );
        }
        assert!(
            !script.contains("diagnostics=\"${2:-$repository/target/web-smoke}\"\nrm -rf"),
            "web-smoke.sh must never remove a caller path unconditionally"
        );
    }

    #[test]
    fn the_browser_gate_refuses_a_caller_owned_diagnostics_path() {
        // The gate is handed the current directory, which is neither inside
        // the repository build root nor a directory the gate created.
        let sandbox = Sandbox::new("web-smoke-cwd");
        fs::write(sandbox.path().join("keep-me.txt"), "caller owned").unwrap();
        fs::create_dir(sandbox.path().join("nested")).unwrap();
        fs::write(sandbox.path().join("nested/keep-me.txt"), "caller owned").unwrap();

        let run = Command::new(repository().join("scripts/web-smoke.sh"))
            .arg(repository().join("target"))
            .arg(".")
            .current_dir(sandbox.path())
            .output()
            .expect("the browser gate should launch");
        let stderr = String::from_utf8_lossy(&run.stderr).into_owned();

        assert!(!run.status.success(), "{stderr}");
        assert!(
            stderr.contains("refusing to write diagnostics outside"),
            "{stderr}"
        );
        assert_eq!(read(sandbox.path().join("keep-me.txt")), "caller owned");
        assert_eq!(
            read(sandbox.path().join("nested/keep-me.txt")),
            "caller owned"
        );
        for sentinel in ["Cargo.toml", "src/sitegen.rs", "scripts/web-smoke.sh"] {
            assert!(
                repository().join(sentinel).exists(),
                "{sentinel} must survive"
            );
        }
    }

    #[test]
    fn the_browser_gate_refuses_a_diagnostics_directory_it_did_not_create() {
        // Inside the build root, but populated by somebody else.
        let sandbox = Sandbox::new_in(repository().join("target"), "web-smoke-foreign");
        fs::write(sandbox.path().join("keep-me.txt"), "not ours").unwrap();

        let run = Command::new(repository().join("scripts/web-smoke.sh"))
            .arg(repository().join("target"))
            .arg(sandbox.path())
            .output()
            .expect("the browser gate should launch");
        let stderr = String::from_utf8_lossy(&run.stderr).into_owned();

        assert!(!run.status.success(), "{stderr}");
        assert!(stderr.contains("this script did not create it"), "{stderr}");
        assert_eq!(read(sandbox.path().join("keep-me.txt")), "not ours");
    }

    #[test]
    fn the_browser_gate_refuses_source_root_and_symlink_diagnostics_paths() {
        let sandbox = Sandbox::new_in(repository().join("target"), "web-smoke-links");
        let link = sandbox.path().join("link");
        std::os::unix::fs::symlink(sandbox.path(), &link).unwrap();

        for candidate in [
            repository().join("src"),
            repository().to_path_buf(),
            Path::new("/").to_path_buf(),
            link,
        ] {
            let run = Command::new(repository().join("scripts/web-smoke.sh"))
                .arg(repository().join("target"))
                .arg(&candidate)
                .output()
                .expect("the browser gate should launch");
            let stderr = String::from_utf8_lossy(&run.stderr).into_owned();

            assert!(!run.status.success(), "{} {stderr}", candidate.display());
            assert!(
                stderr.contains("refusing"),
                "{} {stderr}",
                candidate.display()
            );
            assert!(repository().join("src/sitegen.rs").exists());
        }
    }

    struct Sandbox(std::path::PathBuf);

    impl Sandbox {
        fn new(name: &str) -> Self {
            Self::new_in(std::env::temp_dir(), name)
        }

        fn new_in(parent: impl AsRef<Path>, name: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = parent
                .as_ref()
                .join(format!("midcreek-{name}-{}-{unique}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_web_scripts_are_executable() {
        for relative in ["scripts/build-web.sh", "scripts/web-smoke.sh"] {
            let metadata = fs::metadata(repository().join(relative)).unwrap();
            let mode = std::os::unix::fs::PermissionsExt::mode(&metadata.permissions());
            assert_eq!(mode & 0o111, 0o111, "{relative} must be executable");
        }
    }

    fn locked_version(package: &str) -> String {
        let lock = source("Cargo.lock");
        let marker = format!("name = \"{package}\"\nversion = \"");
        let start = lock.find(&marker).expect("package should be locked") + marker.len();
        lock[start..]
            .split('"')
            .next()
            .expect("locked version should terminate")
            .to_owned()
    }
}

mod verification_publication_contract {
    use super::support::{
        browser_root, green_evidence, prior_gallery, raw_browser, raw_report, verification_root,
    };
    use super::*;
    use midcreek_cs_1::{
        design::{CHARACTER_SHEET_SHA256, KEY_ART_SHA256},
        sitegen::{
            BROWSER_FRAME_FILE, CURRENT_SCREENSHOTS, GALLERY_FILE, GALLERY_FRAMES, GalleryManifest,
            GateStatus, GateSummary, HISTORY_SCREENSHOTS, SCREENSHOTS_ROOT, VERIFICATION_FILE,
            VerificationEvidence, VerificationSummary, WORKER_CROP_FILE, update_gallery,
        },
        verification::{ARTIFACT_NAMES, FrameName, VerificationReport},
    };
    use std::{collections::BTreeSet, path::PathBuf};

    // -----------------------------------------------------------------------
    // Sanitized projection
    // -----------------------------------------------------------------------

    #[test]
    fn the_public_projection_carries_only_declared_evidence() {
        let summary = green_evidence().summary;
        let json = serde_json::to_string(&summary).unwrap();
        let value = serde_json::to_value(&summary).unwrap();

        // The published schema is the whole contract: a projection that grew a
        // field would publish it, whatever the strict shape claims, so the
        // exact key set is asserted against a hand-written list rather than
        // round-tripped through the same shape that produced it.
        assert_eq!(
            keys(&value),
            [
                "browser",
                "camera",
                "failed_stage",
                "frames",
                "gates",
                "hashes",
                "metric_failures",
                "metrics",
                "schema_version",
                "semantic_visual_hash",
                "stages",
                "succeeded",
            ]
        );
        assert_eq!(
            keys(&value["frames"][0]),
            [
                "artifact",
                "camera_settled",
                "camera_yaw_degrees",
                "equipment_on_screen",
                "heading",
                "height",
                "hud_status",
                "name",
                "open_tickets",
                "rack_states",
                "stage",
                "width",
                "worker_crop",
            ]
        );
        assert_eq!(
            keys(&value["browser"]),
            [
                "canvas_height",
                "canvas_width",
                "palette_classes",
                "ready_seconds",
                "sampled_pixels",
                "screenshot",
                "unmatched_share",
            ]
        );
        assert_eq!(
            keys(&value["hashes"]),
            ["asset_sources", "assets", "references", "sources"]
        );
        for forbidden in [
            "command_line",
            "stdout",
            "stderr",
            "environment",
            "failure_reason",
            "/Users/",
            "/home/",
            "RUNNER_TEMP",
            "ground_quadrilateral",
            "player_position",
        ] {
            assert!(!json.contains(forbidden), "{forbidden} leaked into {json}");
        }
    }

    /// Every key of one published object, in sorted order.
    fn keys(value: &serde_json::Value) -> Vec<String> {
        let mut keys = value
            .as_object()
            .unwrap_or_else(|| panic!("expected a published object, got {value}"))
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        keys
    }

    #[test]
    fn the_public_projection_keeps_the_named_gates_hashes_and_camera_facts() {
        let summary = green_evidence().summary;

        assert_eq!(summary.frames.len(), FrameName::ALL.len());
        assert_eq!(summary.camera.msaa_samples, 1);
        assert_eq!(summary.camera.clear_color, "#FF00FF");
        assert_eq!(summary.hashes.sources.len(), 7);
        assert_eq!(summary.hashes.assets.len(), 5);
        assert_eq!(summary.hashes.asset_sources.len(), 5);
        assert_eq!(
            summary
                .hashes
                .references
                .get("docs/reference/cel-shift-key-art.png"),
            Some(&KEY_ART_SHA256.to_owned())
        );
        assert!(summary.succeeded);
        assert!(
            summary.metric_failures.is_empty(),
            "{:?}",
            summary.metric_failures
        );
        assert_eq!(summary.metrics.get("render.frames-captured"), Some(&14.0));
        assert_eq!(summary.metrics.get("browser.palette-classes"), Some(&9.0));
    }

    /// The published hash has to describe the report the game wrote, not the
    /// run that published it: the same report projected from a different
    /// directory, and with or without the browser gate beside it, is the same
    /// visual point, while any change to what the game actually recorded is a
    /// different one. That is what the screenshot history deduplicates on, so
    /// a hash that tracked anything else would either duplicate a point or
    /// swallow a real change.
    #[test]
    fn the_semantic_hash_describes_the_report_and_nothing_around_it() {
        let published = green_evidence().summary.semantic_visual_hash;
        assert_eq!(published.len(), 64, "{published}");
        assert!(
            published
                .chars()
                .all(|value| value.is_ascii_hexdigit() && !value.is_ascii_uppercase()),
            "{published}"
        );

        let relocated = scratch("semantic-hash-relocated");
        copy_verification_fixture(relocated.path());
        assert_eq!(
            hash_of(&raw_report("report.json"), relocated.path()),
            published,
            "the same report published from another directory is the same point"
        );
        assert_eq!(
            hash_of(&raw_report("report.json"), &verification_root()),
            published,
            "the browser gate beside it is not part of the game's own report"
        );

        // A field the projection publishes, and a field it deliberately drops,
        // both belong to the report and both have to move the hash.
        let mut published_field = raw_report("report.json");
        published_field.gameplay.tickets_emitted += 1;
        assert_ne!(hash_of(&published_field, &verification_root()), published);

        let mut dropped_field = raw_report("report.json");
        dropped_field.failure_reason = Some("the capture timed out while writing".to_owned());
        assert_ne!(hash_of(&dropped_field, &verification_root()), published);
        assert!(
            !serde_json::to_string(
                &VerificationEvidence::project(&dropped_field, &verification_root(), None)
                    .unwrap()
                    .summary
            )
            .unwrap()
            .contains("timed out"),
            "the dropped field still never reaches the page"
        );
    }

    fn hash_of(report: &VerificationReport, artifacts: &Path) -> String {
        VerificationEvidence::project(report, artifacts, None)
            .expect("the fixture evidence projects")
            .summary
            .semantic_visual_hash
    }

    /// A browser gate row that is green because a report exists says nothing
    /// about what the browser did. Readiness is published from the measured
    /// seconds against the bound the gate itself enforces.
    #[test]
    fn a_browser_slower_than_the_published_readiness_bound_fails_its_own_gate() {
        let mut gate = raw_browser();
        gate.ready_seconds = 45.0;

        let summary = project_with_browser(&gate).summary;

        let readiness = named_gate(&summary, "Browser readiness");
        assert_eq!(readiness.status, GateStatus::Failed);
        assert_eq!((readiness.passed, readiness.failed), (0, 1));
        assert!(
            summary
                .metric_failures
                .iter()
                .any(|failure| failure.metric == "browser.ready-seconds"),
            "{:?}",
            summary.metric_failures
        );
        assert!(!summary.succeeded);
    }

    /// The same row stays green, and keeps reporting the duration it really
    /// measured, for a browser that was ready in time.
    #[test]
    fn a_browser_ready_within_the_bound_still_publishes_a_passed_readiness_gate() {
        let summary = green_evidence().summary;

        let readiness = named_gate(&summary, "Browser readiness");
        assert_eq!(readiness.status, GateStatus::Passed);
        assert_eq!((readiness.passed, readiness.failed), (1, 0));
        assert_eq!(readiness.duration_ms, 4_820);
    }

    /// A palette row that failed used to publish "0 failed", so the published
    /// matrix said a red gate found nothing wrong with it.
    #[test]
    fn a_canvas_missing_approved_palette_classes_publishes_the_failure_it_found() {
        let mut gate = raw_browser();
        gate.pixels.palette_classes = vec!["floor".to_owned(), "rack".to_owned()];

        let summary = project_with_browser(&gate).summary;

        let palette = named_gate(&summary, "Browser canvas palette");
        assert_eq!(palette.status, GateStatus::Failed);
        assert_eq!(palette.passed, 2);
        assert_eq!(
            palette.failed, 1,
            "a failed gate has to report at least one failure"
        );
        assert!(!summary.succeeded);
    }

    fn project_with_browser(
        gate: &midcreek_cs_1::sitegen::BrowserGateReport,
    ) -> VerificationEvidence {
        VerificationEvidence::project(
            &raw_report("report.json"),
            &verification_root(),
            Some((gate, browser_root().as_path())),
        )
        .expect("the fixture evidence projects")
    }

    fn named_gate<'summary>(
        summary: &'summary VerificationSummary,
        name: &str,
    ) -> &'summary GateSummary {
        summary
            .gates
            .iter()
            .find(|gate| gate.name == name)
            .unwrap_or_else(|| panic!("expected a {name:?} gate in {:?}", summary.gates))
    }

    #[test]
    fn a_raw_report_carrying_undeclared_fields_is_refused() {
        let mut raw: serde_json::Value =
            serde_json::from_str(&read(verification_root().join("report.json"))).unwrap();
        let object = raw.as_object_mut().unwrap();
        object.insert(
            "command_line".to_owned(),
            serde_json::json!("/Users/runner/target/debug/midcreek-cs-1 --verify-output /tmp/x"),
        );
        object.insert(
            "environment".to_owned(),
            serde_json::json!({ "GITHUB_TOKEN": "ghp_example" }),
        );

        let parsed = serde_json::from_str::<VerificationReport>(&raw.to_string());

        let message = parsed
            .expect_err("hostile fields must be refused")
            .to_string();
        assert!(message.contains("unknown field"), "{message}");
    }

    #[test]
    fn a_raw_browser_gate_carrying_undeclared_fields_is_refused() {
        let mut raw: serde_json::Value =
            serde_json::from_str(&read(browser_root().join("browser-gate.json"))).unwrap();
        raw.as_object_mut().unwrap().insert(
            "chrome_path".to_owned(),
            serde_json::json!("/usr/bin/google-chrome"),
        );

        let parsed =
            serde_json::from_str::<midcreek_cs_1::sitegen::BrowserGateReport>(&raw.to_string());

        assert!(parsed.is_err(), "the browser gate shape must be strict");
    }

    #[test]
    fn an_artifact_path_that_escapes_the_artifact_root_is_refused() {
        let mut report = raw_report("report.json");
        let name = FrameName::HealthyCenterNorthEast.file_name();
        report.frames.get_mut(name).unwrap().path = "../../../etc/passwd".to_owned();

        let result = VerificationEvidence::project(&report, &verification_root(), None);

        assert!(
            matches!(&result, Err(SitegenError::UntrustedArtifact { path })
                if path == Path::new("../../../etc/passwd")),
            "{:?}",
            result.err()
        );
    }

    #[test]
    fn an_absolute_artifact_path_is_refused() {
        let mut report = raw_report("report.json");
        let name = FrameName::HealthyCenterNorthEast.file_name();
        report.frames.get_mut(name).unwrap().path = "/etc/passwd".to_owned();

        let result = VerificationEvidence::project(&report, &verification_root(), None);

        assert!(
            matches!(&result, Err(SitegenError::UntrustedArtifact { .. })),
            "{:?}",
            result.err()
        );
    }

    #[test]
    fn an_artifact_symlink_is_refused() {
        let root = scratch("symlinked-artifacts");
        copy_verification_fixture(root.path());
        let name = FrameName::HealthyCenterNorthEast.file_name();
        fs::remove_file(root.path().join(name)).unwrap();
        std::os::unix::fs::symlink(verification_root().join(name), root.path().join(name)).unwrap();

        let result = VerificationEvidence::project(&raw_report("report.json"), root.path(), None);

        assert!(
            matches!(&result, Err(SitegenError::UntrustedArtifact { .. })),
            "{:?}",
            result.err()
        );
    }

    /// A relative path made only of normal components, naming a real regular
    /// file that is not itself a link, still escapes its root when a directory
    /// above it is a link. Only canonicalizing both sides catches it.
    #[test]
    fn an_artifact_below_a_symlinked_ancestor_that_escapes_the_root_is_refused() {
        let root = scratch("symlinked-ancestor-root");
        let outside = scratch("symlinked-ancestor-target");
        copy_verification_fixture(root.path());

        let name = FrameName::WalkNorthEast.file_name();
        let frames = outside.path().join("frames");
        fs::create_dir_all(&frames).unwrap();
        fs::copy(verification_root().join(name), frames.join(name)).unwrap();
        std::os::unix::fs::symlink(&frames, root.path().join("frames")).unwrap();

        let declared = format!("frames/{name}");
        let mut report = raw_report("report.json");
        report.frames.get_mut(name).unwrap().path = declared.clone();

        // Every check that precedes canonicalization is satisfied: the path is
        // relative, carries only normal components, and names a regular file
        // that is not a link.
        assert!(
            Path::new(&declared)
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
        );
        let reached = fs::symlink_metadata(root.path().join(&declared)).unwrap();
        assert!(reached.is_file() && !reached.file_type().is_symlink());
        assert_eq!(
            fs::read(root.path().join(&declared)).unwrap(),
            fs::read(verification_root().join(name)).unwrap(),
            "the artifact is otherwise entirely valid"
        );

        let result = VerificationEvidence::project(&report, root.path(), None);

        assert!(
            matches!(&result, Err(SitegenError::UntrustedArtifact { path })
                if path == Path::new(&declared)),
            "{:?}",
            result.err()
        );
    }

    #[test]
    fn a_missing_artifact_is_refused() {
        let root = scratch("missing-artifact");
        copy_verification_fixture(root.path());
        fs::remove_file(root.path().join(FrameName::ResolvedNorthEast.file_name())).unwrap();

        let result = VerificationEvidence::project(&raw_report("report.json"), root.path(), None);

        assert!(
            matches!(&result, Err(SitegenError::MissingInput { path })
                if path.file_name().is_some_and(|name| name == "05-resolved-ne.png")),
            "{:?}",
            result.err()
        );
    }

    #[test]
    fn a_corrupt_artifact_is_refused() {
        let root = scratch("corrupt-artifact");
        copy_verification_fixture(root.path());
        fs::write(
            root.path().join(FrameName::WalkNorthEast.file_name()),
            b"\x89PNG\r\n\x1a\nnot actually an image",
        )
        .unwrap();

        let result = VerificationEvidence::project(&raw_report("report.json"), root.path(), None);

        assert!(
            matches!(&result, Err(SitegenError::CorruptArtifact { path, .. })
                if path.file_name().is_some_and(|name| name == "03-walk-ne.png")),
            "{:?}",
            result.err()
        );
    }

    #[test]
    fn an_artifact_whose_pixels_disagree_with_the_report_is_refused() {
        let mut report = raw_report("report.json");
        report
            .frames
            .get_mut(FrameName::MidOrbit.file_name())
            .unwrap()
            .width = 640;

        let result = VerificationEvidence::project(&report, &verification_root(), None);

        assert!(
            matches!(&result, Err(SitegenError::CorruptArtifact { path, .. })
                if path.file_name().is_some_and(|name| name == "09-mid-orbit.png")),
            "{:?}",
            result.err()
        );
    }

    #[test]
    fn a_report_whose_reference_hashes_disagree_with_the_approved_manifest_is_refused() {
        let mut report = raw_report("report.json");
        report.references.insert(
            "docs/reference/cel-shift-key-art.png".to_owned(),
            "0".repeat(64),
        );

        let result = VerificationEvidence::project(&report, &verification_root(), None);

        assert!(
            matches!(&result, Err(SitegenError::ReferenceProvenance { .. })),
            "{:?}",
            result.err()
        );
    }

    // -----------------------------------------------------------------------
    // Gallery ordering and deduplication
    // -----------------------------------------------------------------------

    #[test]
    fn adds_gallery_entry_only_when_visual_hash_changes() {
        let existing = prior_gallery();
        let latest = existing
            .entries
            .last()
            .unwrap()
            .semantic_visual_hash
            .clone();

        let same = update_gallery(
            &existing,
            &report_with_hash(&latest),
            &commit_summary("3333333333333333333333333333333333333333"),
        );
        assert_eq!(same.entries.len(), 1);
        assert_eq!(same, existing);

        let changed = update_gallery(
            &existing,
            &report_with_hash(&"d".repeat(64)),
            &commit_summary("3333333333333333333333333333333333333333"),
        );
        assert_eq!(changed.entries.len(), 2);
        assert_eq!(
            changed.entries[0].semantic_visual_hash,
            existing.entries[0].semantic_visual_hash
        );
        assert_eq!(changed.entries[1].source_commit, "3".repeat(40));
    }

    #[test]
    fn a_gallery_entry_records_frames_metrics_and_deltas_against_the_previous_entry() {
        let existing = prior_gallery();
        let mut summary = report_with_hash(&"e".repeat(64));
        summary
            .metrics
            .insert("gameplay.tickets-emitted".to_owned(), 5.0);

        let updated = update_gallery(
            &existing,
            &summary,
            &commit_summary("3333333333333333333333333333333333333333"),
        );

        let entry = updated.entries.last().unwrap();
        assert_eq!(entry.current_task, "autonomous-assets");
        assert_eq!(entry.committed_at, "2026-08-30T09:00:00Z");
        assert_eq!(
            entry.metric_deltas.get("gameplay.tickets-emitted"),
            Some(&2.0)
        );
        for (label, file) in GALLERY_FRAMES {
            assert_eq!(
                entry.frames.get(label).map(String::as_str),
                Some(format!("{HISTORY_SCREENSHOTS}/33333333/{file}").as_str()),
                "{label}"
            );
        }
        assert_eq!(
            entry.frames.get("worker").map(String::as_str),
            Some(format!("{HISTORY_SCREENSHOTS}/33333333/{WORKER_CROP_FILE}").as_str())
        );
    }

    /// Only the *latest* entry's hash suppresses a new point, so a revert to
    /// pixels the history already recorded earlier opens a new point rather
    /// than reusing the old one. That is the documented behaviour: the history
    /// is a timeline of what was published when, not a set of distinct
    /// appearances, and a revert really is a later moment at which the project
    /// looked that way.
    #[test]
    fn reverting_to_an_older_visual_hash_opens_a_new_history_point() {
        let original = prior_gallery();
        let first_hash = original.entries[0].semantic_visual_hash.clone();
        let moved_on = update_gallery(
            &original,
            &report_with_hash(&"b".repeat(64)),
            &commit_summary("3333333333333333333333333333333333333333"),
        );
        assert_eq!(moved_on.entries.len(), 2);

        let reverted = update_gallery(
            &moved_on,
            &report_with_hash(&first_hash),
            &commit_summary("4444444444444444444444444444444444444444"),
        );

        assert_eq!(reverted.entries.len(), 3);
        let entry = reverted.entries.last().unwrap();
        assert_eq!(entry.semantic_visual_hash, first_hash);
        assert_eq!(entry.source_commit, "4".repeat(40));

        let mut inputs = site_inputs("verified-game");
        inputs.gallery = Some(moved_on);
        inputs
            .verification
            .as_mut()
            .unwrap()
            .summary
            .semantic_visual_hash = first_hash;
        let source_commit = inputs.workflow.source_commit.clone();
        let html = build_site_from_inputs("rendered-revert", &inputs)
            .unwrap()
            .index_html();
        let document = scraper::Html::parse_document(&html);
        let entries = scraper::Selector::parse("#screenshots .screenshot-entry").unwrap();
        let new_entries = document
            .select(&entries)
            .filter(|entry| entry.text().any(|text| text.contains("New this build")))
            .collect::<Vec<_>>();

        assert_eq!(
            new_entries.len(),
            1,
            "only the appended history point may carry the badge: {html}"
        );
        assert!(
            new_entries[0]
                .text()
                .any(|text| text.contains(&source_commit[..8])),
            "the badge must identify the appended commit: {html}"
        );
    }

    #[test]
    fn a_repeated_source_commit_never_duplicates_a_gallery_entry() {
        let existing = prior_gallery();
        let commit = existing.entries[0].source_commit.clone();

        let updated = update_gallery(
            &existing,
            &report_with_hash(&"f".repeat(64)),
            &commit_summary(&commit),
        );

        assert_eq!(updated, existing);
    }

    #[test]
    fn a_failed_verification_leaves_the_previous_gallery_unchanged() {
        let existing = prior_gallery();
        let mut summary = report_with_hash(&"a".repeat(64));
        summary.succeeded = false;

        let updated = update_gallery(
            &existing,
            &summary,
            &commit_summary("3333333333333333333333333333333333333333"),
        );

        assert_eq!(updated, existing);
    }

    // -----------------------------------------------------------------------
    // Screenshot promotion
    // -----------------------------------------------------------------------

    #[test]
    fn a_green_build_promotes_every_verification_frame_and_the_browser_proof() {
        let site = build_fixture_site("verified-game").unwrap();
        let current = site.root().join(CURRENT_SCREENSHOTS);

        for frame in FrameName::ALL {
            assert!(
                current.join(frame.file_name()).is_file(),
                "{} was not promoted",
                frame.file_name()
            );
        }
        assert!(current.join(WORKER_CROP_FILE).is_file());
        assert!(current.join(BROWSER_FRAME_FILE).is_file());
        assert!(site.root().join(VERIFICATION_FILE).is_file());
    }

    #[test]
    fn the_promoted_frames_are_byte_identical_to_the_verified_artifacts() {
        let site = build_fixture_site("verified-game").unwrap();

        for frame in FrameName::ALL {
            assert_eq!(
                sha256(
                    site.root()
                        .join(CURRENT_SCREENSHOTS)
                        .join(frame.file_name())
                ),
                sha256(verification_root().join(frame.file_name())),
                "{} was altered on publication",
                frame.file_name()
            );
        }
    }

    #[test]
    fn only_the_declared_verification_artifacts_are_copied() {
        let root = scratch("extra-artifacts");
        copy_verification_fixture(root.path());
        fs::write(root.path().join("stderr.log"), "thread 'main' panicked").unwrap();
        fs::write(root.path().join("stdout.log"), "/Users/runner/work").unwrap();
        let mut inputs = site_inputs("verified-game");
        inputs.verification = Some(
            VerificationEvidence::project(&raw_report("report.json"), root.path(), None).unwrap(),
        );

        let site = build_site_from_inputs("extra-artifacts", &inputs).unwrap();

        let published = fs::read_dir(site.root().join(CURRENT_SCREENSHOTS))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<BTreeSet<_>>();
        assert!(!published.contains("stderr.log"));
        assert!(!published.contains("stdout.log"));
        assert!(!published.contains(midcreek_cs_1::verification::REPORT_FILE_NAME));
        assert_eq!(published.len(), FrameName::ALL.len() + 1);
        assert!(ARTIFACT_NAMES.contains(&"report.json"));
    }

    #[test]
    fn the_worker_crop_is_the_reported_rectangle_of_the_center_frame() {
        let site = build_fixture_site("verified-game").unwrap();

        let crop = image::open(site.root().join(CURRENT_SCREENSHOTS).join(WORKER_CROP_FILE))
            .unwrap()
            .to_rgb8();
        let center =
            image::open(verification_root().join(FrameName::HealthyCenterNorthEast.file_name()))
                .unwrap()
                .to_rgb8();
        let reported = green_evidence().summary.frames[0].worker_crop;

        assert_eq!(
            (crop.width(), crop.height()),
            (reported.width, reported.height)
        );
        assert_eq!((crop.width(), crop.height()), (40, 90));
        for y in 0..crop.height() {
            for x in 0..crop.width() {
                assert_eq!(
                    crop.get_pixel(x, y),
                    center.get_pixel(reported.x + x, reported.y + y),
                    "the crop must be the reported rectangle at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn a_changed_visual_hash_publishes_one_new_history_entry() {
        let site = build_fixture_site("verified-game").unwrap();
        let gallery: GalleryManifest =
            serde_json::from_str(&read(site.root().join(GALLERY_FILE))).unwrap();

        assert_eq!(gallery.entries.len(), 2);
        let entry = gallery.entries.last().unwrap();
        assert_eq!(entry.source_commit, "1".repeat(40));
        for path in entry.frames.values() {
            assert!(
                site.root().join(path).is_file(),
                "{path} was recorded but never published"
            );
        }
    }

    #[test]
    fn an_unchanged_visual_hash_deduplicates_but_still_publishes_the_current_frames() {
        let mut inputs = site_inputs("verified-game");
        let hash = inputs
            .verification
            .as_ref()
            .unwrap()
            .summary
            .semantic_visual_hash
            .clone();
        let mut gallery = prior_gallery();
        gallery.entries[0].semantic_visual_hash = hash;
        inputs.gallery = Some(gallery);

        let site = build_site_from_inputs("unchanged-hash", &inputs).unwrap();

        let published: GalleryManifest =
            serde_json::from_str(&read(site.root().join(GALLERY_FILE))).unwrap();
        assert_eq!(published.entries.len(), 1);
        assert!(
            !site
                .root()
                .join(HISTORY_SCREENSHOTS)
                .join("11111111")
                .exists(),
            "an unchanged hash must not open a history entry"
        );
        assert!(
            site.root()
                .join(CURRENT_SCREENSHOTS)
                .join(FrameName::HealthyCenterNorthEast.file_name())
                .is_file(),
            "the current frame is retained even without a new history entry"
        );
    }

    #[test]
    fn a_failed_verification_publishes_no_screenshots_or_gallery() {
        let site = build_fixture_site("failed-verification").unwrap();

        assert!(
            !site.root().join("screenshots").exists(),
            "a failed run must leave the retained screenshots alone"
        );
        assert!(!site.root().join(GALLERY_FILE).exists());
        assert!(site.root().join(VERIFICATION_FILE).is_file());
    }

    // -----------------------------------------------------------------------
    // Rendered evidence
    // -----------------------------------------------------------------------

    #[test]
    fn renders_gate_counts_and_durations() {
        let site = build_fixture_site("verified-game").unwrap();
        let html = site.index_html();

        assert_text(&html, "#tests", "Verified frame captures");
        assert_text(&html, "#tests", "14 passed");
        assert_text(&html, "#tests", "Browser readiness");
        assert_text(&html, "#tests", "4.82 s");
    }

    /// The projection reads the run's own report. It can vouch for the frames
    /// the run really captured, but it never re-runs `evaluate_frame`, so it
    /// must not publish the render contract's verdict as if it had. The
    /// verdict itself reaches the site as a workflow gate, from the job that
    /// really ran the analyzers.
    #[test]
    fn the_published_gates_never_claim_the_render_image_contract_passed() {
        let site = build_fixture_site("verified-game").unwrap();
        let html = site.index_html();
        let summary =
            serde_json::from_str::<VerificationSummary>(&read(site.root().join(VERIFICATION_FILE)))
                .unwrap();

        assert!(
            !html.contains("Rendered image contracts"),
            "the report alone cannot vouch for the reference image analyzers"
        );
        assert!(
            summary
                .gates
                .iter()
                .all(|gate| gate.name != "Rendered image contracts"),
            "{:?}",
            summary.gates
        );
        assert!(
            summary
                .gates
                .iter()
                .any(|gate| gate.name == "Verified frame captures" && gate.passed == 14),
            "{:?}",
            summary.gates
        );
    }

    /// `Rendered image contracts` is a real verdict, so it is published only
    /// when the workflow result carries the outcome and the duration of the
    /// job step that really ran the serialized render contract.
    #[test]
    fn the_render_image_contract_verdict_is_published_from_the_workflow_result() {
        let mut inputs = site_inputs("verified-game");
        inputs.workflow.gates.push(GateSummary {
            name: "Rendered image contracts".to_owned(),
            status: GateStatus::Passed,
            passed: 1,
            failed: 0,
            duration_ms: 184_000,
            artifact_url: None,
        });
        let site = build_site_from_inputs("render-verdict", &inputs).unwrap();
        let html = site.index_html();
        let summary =
            serde_json::from_str::<VerificationSummary>(&read(site.root().join(VERIFICATION_FILE)))
                .unwrap();

        assert_text(&html, "#tests", "Rendered image contracts");
        assert_text(&html, "#tests", "184.00 s");
        assert_text(&html, "#tests", "Verified frame captures");
        assert!(
            summary
                .gates
                .iter()
                .all(|gate| gate.name != "Rendered image contracts"),
            "the projection still holds no analyzer verdict: {:?}",
            summary.gates
        );
    }

    /// A failed render job publishes its own red row rather than borrowing the
    /// report's account of the frames it captured.
    #[test]
    fn a_failed_render_image_contract_is_published_as_failed() {
        let mut inputs = site_inputs("verified-game");
        inputs.workflow.gates.push(GateSummary {
            name: "Rendered image contracts".to_owned(),
            status: GateStatus::Failed,
            passed: 0,
            failed: 1,
            duration_ms: 12_000,
            artifact_url: None,
        });
        let html = build_site_from_inputs("render-verdict-failed", &inputs)
            .unwrap()
            .index_html();

        let row = html
            .split("Rendered image contracts")
            .nth(1)
            .expect("the failed gate should render a row");
        assert!(row.contains("gate-failed"), "{row}");
    }

    #[test]
    fn comparison_page_includes_reference_provenance() {
        let html = build_fixture_site("verified-game").unwrap().index_html();

        assert!(html.contains(KEY_ART_SHA256), "{html}");
        assert!(html.contains(CHARACTER_SHEET_SHA256));
        assert!(html.contains("a30e12b63a36743015b1c73eeca6248"));
        assert!(html.contains("8a5a31e7bceb8ad16b3481d2bae89e7"));
        assert_text(&html, "#comparison", "docs/reference/cel-shift-key-art.png");
    }

    #[test]
    fn the_comparison_slider_shows_the_current_frame_against_the_key_art() {
        let site = build_fixture_site("verified-game").unwrap();
        let html = site.index_html();
        let document = scraper::Html::parse_document(&html);
        let selector = scraper::Selector::parse("#comparison [data-comparison] img").unwrap();
        let sources = document
            .select(&selector)
            .map(|image| {
                let alt = image.value().attr("alt").unwrap_or_default().to_owned();
                assert!(
                    !alt.trim().is_empty(),
                    "every comparison image needs alt text"
                );
                image.value().attr("src").unwrap_or_default().to_owned()
            })
            .collect::<Vec<_>>();

        assert_eq!(sources.len(), 2);
        assert!(sources.contains(&"reference/cel-shift-key-art.png".to_owned()));
        assert!(sources.contains(&format!(
            "{CURRENT_SCREENSHOTS}/{}",
            FrameName::HealthyCenterNorthEast.file_name()
        )));
        assert!(!html.contains("No verified current frame"));
        assert!(html.contains("data-compare-control"));
    }

    #[test]
    fn the_worker_crop_is_shown_beside_the_character_sheet() {
        let html = build_fixture_site("verified-game").unwrap().index_html();
        let document = scraper::Html::parse_document(&html);
        let selector = scraper::Selector::parse("#comparison .character-comparison img").unwrap();
        let sources = document
            .select(&selector)
            .map(|image| image.value().attr("src").unwrap_or_default().to_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            sources,
            [
                "reference/cel-shift-character-sheet.png".to_owned(),
                format!("{CURRENT_SCREENSHOTS}/{WORKER_CROP_FILE}"),
            ]
        );
        assert!(!html.contains("No verified worker crop"));
    }

    #[test]
    fn renders_the_metric_table_with_values_and_deltas() {
        let html = build_fixture_site("verified-game").unwrap().index_html();

        assert_text(&html, "#tests", "render.frames-captured");
        assert_text(&html, "#tests", "browser.ready-seconds");
        assert_text(&html, "#tests", "gameplay.tickets-emitted");
        assert_text(&html, "#tests", "Change");
    }

    #[test]
    fn renders_the_exact_source_paths_and_hashes_of_the_verified_run() {
        let html = build_fixture_site("verified-game").unwrap().index_html();
        let summary = green_evidence().summary;

        assert_text(&html, "#tests", "src/verification.rs");
        assert_text(&html, "#tests", "assets/generated/rack.glb");
        assert_text(&html, "#tests", &summary.semantic_visual_hash);
        assert!(html.contains(summary.hashes.sources["src/verification.rs"].as_str()));
    }

    /// A history point can move a dozen metrics at once, so the list has to
    /// lead with the movement a reader is looking for rather than with
    /// whichever metric name sorts first.
    #[test]
    fn the_history_lists_the_largest_metric_movement_first() {
        let mut inputs = site_inputs("green");
        let mut gallery = prior_gallery();
        gallery.entries[0].metric_deltas = [
            ("aaa.tiny", 0.25),
            ("mmm.middling", -5.0),
            ("zzz.largest", 42.0),
            ("bbb.unmoved", 0.0),
        ]
        .into_iter()
        .map(|(name, delta)| (name.to_owned(), delta))
        .collect();
        inputs.gallery = Some(gallery);

        let html = build_site_from_inputs("delta-order", &inputs)
            .unwrap()
            .index_html();
        let document = scraper::Html::parse_document(&html);
        let selector = scraper::Selector::parse("#screenshots .delta-list code").unwrap();
        let order = document
            .select(&selector)
            .map(|code| code.text().collect::<String>())
            .collect::<Vec<_>>();

        assert_eq!(
            order,
            ["zzz.largest", "mmm.middling", "aaa.tiny"],
            "the list must be ordered by how far each metric moved"
        );
    }

    /// Every pixel a build promotes is evidence somebody has to be able to
    /// look at. A frame copied into the published tree but linked from
    /// nowhere is weight the site serves and nobody can see.
    #[test]
    fn every_promoted_pixel_is_linked_from_the_published_page() {
        let site = build_fixture_site("verified-game").unwrap();
        let html = site.index_html();
        let document = scraper::Html::parse_document(&html);
        let selector = scraper::Selector::parse("img[src], a[href], iframe[src]").unwrap();
        let linked = document
            .select(&selector)
            .filter_map(|element| {
                element
                    .value()
                    .attr("src")
                    .or_else(|| element.value().attr("href"))
            })
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();

        let mut promoted = Vec::new();
        collect_relative(
            &site.root().join(SCREENSHOTS_ROOT),
            site.root(),
            &mut promoted,
        );
        promoted.sort();

        assert!(
            promoted.len() > GALLERY_FRAMES.len(),
            "this fixture promotes every captured frame: {promoted:?}"
        );
        for file in &promoted {
            let target = file.to_string_lossy().into_owned();
            assert!(
                linked.contains(&target),
                "{target} was promoted but the page links nothing to it"
            );
        }
    }

    /// The other direction of the same rule: the current-frame strip may only
    /// point at pixels this build itself copied. The projection the strip used
    /// to be rendered from is an input that describes what a run *reported*;
    /// the publication record describes what was really written, and only the
    /// second one can vouch for a link. When the two disagree the page has to
    /// follow the copies, because a visitor can only open a file that exists.
    #[test]
    fn the_current_frame_strip_links_exactly_the_frames_this_build_copied() {
        let site = build_fixture_site("verified-game").unwrap();
        let document = scraper::Html::parse_document(&site.index_html());
        let selector = scraper::Selector::parse("#screenshots img[src]").unwrap();
        let linked = document
            .select(&selector)
            .filter_map(|image| image.value().attr("src"))
            .filter(|source| source.starts_with(&format!("{CURRENT_SCREENSHOTS}/")))
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();

        let mut published = Vec::new();
        collect_relative(
            &site.root().join(CURRENT_SCREENSHOTS),
            site.root(),
            &mut published,
        );
        // The technician crop and the browser canvas are derived proofs shown
        // beside the references they are compared with, not captures of the
        // run, so the strip is the whole of the rest.
        let captured = published
            .iter()
            .map(|file| file.to_string_lossy().into_owned())
            .filter(|file| !file.ends_with(WORKER_CROP_FILE) && !file.ends_with(BROWSER_FRAME_FILE))
            .collect::<BTreeSet<_>>();

        assert!(captured.len() > 1, "{captured:?}");
        assert_eq!(
            linked, captured,
            "the strip must link the frames this build copied, no more and no less"
        );
    }

    #[test]
    fn renders_the_screenshot_history_with_accessible_images() {
        let site = build_fixture_site("verified-game").unwrap();
        let html = site.index_html();
        let gallery: GalleryManifest =
            serde_json::from_str(&read(site.root().join(GALLERY_FILE))).unwrap();
        let vouched = gallery
            .entries
            .iter()
            .flat_map(|entry| entry.frames.values().cloned())
            .collect::<BTreeSet<_>>();
        let document = scraper::Html::parse_document(&html);
        let selector = scraper::Selector::parse("#screenshots img").unwrap();
        let images = document.select(&selector).collect::<Vec<_>>();

        assert!(!images.is_empty(), "a green run publishes a visual history");
        for image in images {
            let alt = image.value().attr("alt").unwrap_or_default();
            let source = image.value().attr("src").unwrap_or_default();
            assert!(!alt.trim().is_empty(), "history images need alt text");
            assert!(!source.starts_with('/'), "{source} must stay relative");
            // Older accepted points are retained by assembly, so the published
            // manifest is what vouches for pixels this build did not write.
            assert!(
                site.root().join(source).is_file() || vouched.contains(source),
                "{source} is neither published nor declared"
            );
        }
        assert_text(&html, "#screenshots", "11111111");
        assert_text(&html, "#screenshots", "22222222");
    }

    #[test]
    fn a_history_image_the_gallery_does_not_declare_is_still_a_broken_link() {
        let site = build_fixture_site("verified-game").unwrap();
        let index = site.root().join("index.html");
        let html = read(&index).replace(
            &format!("{HISTORY_SCREENSHOTS}/22222222/"),
            &format!("{HISTORY_SCREENSHOTS}/undeclared/"),
        );
        fs::write(&index, html).unwrap();

        let result = midcreek_cs_1::sitegen::validate_site_output(
            site.root(),
            &site_inputs("verified-game").progress,
        );

        assert!(
            matches!(&result, Err(SitegenError::BrokenLocalLink { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn the_publication_mode_names_what_the_page_actually_carries() {
        let verified = build_fixture_site("verified-game").unwrap().index_html();
        let failed = build_fixture_site("failed-verification")
            .unwrap()
            .index_html();
        let status_only = build_fixture_site("green").unwrap().index_html();

        assert_text(&verified, ".hero-badge", "Evidence");
        assert_text(&failed, ".hero-badge", "Status");
        assert_text(&status_only, ".hero-badge", "Status");
        assert!(!verified.contains("status-only phase"));
    }

    #[test]
    fn links_full_logs_to_github_actions_rather_than_publishing_them() {
        let html = build_fixture_site("verified-game").unwrap().index_html();

        assert!(html.contains("https://github.com/ridermw/midcreek-cs-1/actions/runs/456"));
        assert_text(&html, "#tests", "Open the workflow run");
    }

    #[test]
    fn a_failed_run_publishes_failed_metric_names_and_values_without_raw_logs() {
        let html = build_fixture_site("failed-verification")
            .unwrap()
            .index_html();

        assert_text(&html, "#tests", "render.frames-captured");
        assert_text(&html, "#tests", "repair-capture");
        assert!(!html.contains("capture timed out while writing"), "{html}");
        assert!(!html.contains("/Users/runner"), "{html}");
        assert!(!html.contains("target/render-contract"), "{html}");
    }

    #[test]
    fn a_failed_run_keeps_the_previous_history_visible_without_broken_images() {
        let site = build_fixture_site("failed-verification").unwrap();
        let html = site.index_html();
        let document = scraper::Html::parse_document(&html);
        let selector = scraper::Selector::parse("#screenshots img").unwrap();

        assert_text(&html, "#screenshots", "22222222");
        assert_eq!(
            document.select(&selector).count(),
            0,
            "a failed build must not link images it did not publish"
        );
    }

    #[test]
    fn no_local_path_or_raw_log_reaches_any_generated_page() {
        for fixture in ["verified-game", "failed-verification"] {
            let site = build_fixture_site(fixture).unwrap();
            for relative in ["index.html", VERIFICATION_FILE] {
                let path = site.root().join(relative);
                if !path.is_file() {
                    continue;
                }
                let text = read(&path);
                for forbidden in [
                    env!("CARGO_MANIFEST_DIR"),
                    "/Users/",
                    "/home/",
                    "file://",
                    "stderr.log",
                    "panicked",
                ] {
                    assert!(
                        !text.contains(forbidden),
                        "{fixture}/{relative} leaked {forbidden}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_build_cli_renders_a_verified_preview_site() {
        let output = cli_build(
            "tests/fixtures/sitegen/verified-game/inputs.json",
            "verified",
        );

        assert!(output.join(GALLERY_FILE).is_file());
        assert!(
            output
                .join(CURRENT_SCREENSHOTS)
                .join(FrameName::HealthyCenterNorthEast.file_name())
                .is_file()
        );
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn the_build_cli_renders_a_failure_preview_site() {
        let output = cli_build(
            "tests/fixtures/sitegen/failed-verification/inputs.json",
            "failed",
        );

        assert!(!output.join("screenshots").exists());
        assert!(read(output.join("index.html")).contains("repair-capture"));
        fs::remove_dir_all(output).unwrap();
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn cli_build(inputs: &str, name: &str) -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let output = std::env::temp_dir().join(format!(
            "midcreek-sitegen-preview-{name}-{}-{unique}",
            std::process::id()
        ));
        let result = Command::new(env!("CARGO_BIN_EXE_sitegen"))
            .current_dir(root)
            .args([
                "build",
                "--inputs",
                inputs,
                "--output",
                output.to_str().unwrap(),
            ])
            .output()
            .expect("sitegen should launch");
        assert_eq!(
            result.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        output
    }

    fn report_with_hash(hash: &str) -> VerificationSummary {
        let mut summary = green_evidence().summary;
        summary.semantic_visual_hash = hash.to_owned();
        summary
    }

    fn commit_summary(sha: &str) -> midcreek_cs_1::sitegen::CommitSummary {
        midcreek_cs_1::sitegen::CommitSummary {
            sha: sha.to_owned(),
            subject: "Publish verification evidence".to_owned(),
            committed_at: "2026-08-30T09:00:00Z".to_owned(),
            task_id: Some("autonomous-assets".to_owned()),
        }
    }

    /// A build that publishes both domains at once: the packaged game and the
    /// promoted verification frames.
    #[test]
    fn last_green_metadata_enumerates_the_screenshots_it_actually_promoted() {
        let package = scratch("last-green-package");
        for (relative, contents) in [
            ("index.html", "<!doctype html><html><body></body></html>"),
            ("game.js", "export default function init() {}"),
            ("game_bg.wasm", "\0asm"),
            ("play.js", "// bootstrap"),
            ("play.css", "/* shell */"),
            ("assets/generated/rack.glb", "glTF"),
        ] {
            let path = package.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        let mut inputs = site_inputs("verified-game");
        inputs.playable = Some(midcreek_cs_1::sitegen::PlayableBuild {
            directory: package.path().to_path_buf(),
            source_commit: "1111111111111111111111111111111111111111".to_owned(),
            run_url: "https://example.invalid/run/1".to_owned(),
        });

        let site = build_site_from_inputs("last-green-evidence", &inputs).unwrap();
        let metadata = serde_json::from_str::<midcreek_cs_1::sitegen::LastGreenManifest>(&read(
            site.root().join("last-green.json"),
        ))
        .unwrap();
        let summary =
            serde_json::from_str::<VerificationSummary>(&read(site.root().join(VERIFICATION_FILE)))
                .unwrap();

        assert_eq!(
            metadata.semantic_visual_hash.as_deref(),
            Some(summary.semantic_visual_hash.as_str())
        );
        for file in &metadata.screenshot_files {
            assert!(
                site.root().join(file).is_file(),
                "last-green.json names {file:?}, which was never published"
            );
        }

        let mut published = Vec::new();
        collect_relative(
            &site.root().join(CURRENT_SCREENSHOTS),
            site.root(),
            &mut published,
        );
        published.sort();
        assert!(
            !published.is_empty(),
            "this fixture promotes a full set of current frames"
        );
        assert_eq!(
            metadata.screenshot_files, published,
            "every current screenshot is listed exactly once"
        );
        assert!(
            site.root().join(HISTORY_SCREENSHOTS).is_dir(),
            "this fixture also opens a history point, so the distinction is real"
        );
        assert!(
            metadata
                .screenshot_files
                .iter()
                .all(|file| !file.starts_with(HISTORY_SCREENSHOTS)),
            "history frames are not the current screenshots"
        );
    }

    /// Every regular file below `root`, relative to `base`.
    fn collect_relative(root: &Path, base: &Path, found: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(root) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect_relative(&path, base, found);
            } else {
                found.push(path.strip_prefix(base).unwrap().to_path_buf());
            }
        }
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(name: &str) -> Scratch {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/verification-fixtures")
            .join(format!("{name}-{}-{unique}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        Scratch(root)
    }

    fn copy_verification_fixture(destination: &Path) {
        for entry in fs::read_dir(verification_root()).unwrap() {
            let entry = entry.unwrap();
            fs::copy(entry.path(), destination.join(entry.file_name())).unwrap();
        }
    }
}

/// The generated README status block, the `check` gate that maintains it, and
/// the `readme` command that regenerates it.
mod readme_status_contract {
    use std::{
        path::PathBuf,
        process::{Command, Output, Stdio},
    };

    use midcreek_cs_1::sitegen::{
        Challenge, ChallengeStatus, ProgressDocument, ProgressStatus, ProgressTask,
        README_STATUS_END, README_STATUS_START, ReadmeStatusError, check_readme_status,
        readme_status_block, render_readme_status, replace_readme_status,
    };

    use super::*;

    // -----------------------------------------------------------------------
    // What the generated block says
    // -----------------------------------------------------------------------

    /// The published facts are read from the parsed progress document and the
    /// plan; the identity rows are digests of the stored source bytes. The two
    /// are deliberately independent, so this test hands over sources whose
    /// bytes say nothing about the document, and hand-computed digests of
    /// exactly those bytes.
    #[test]
    fn the_generated_block_publishes_the_current_task_counts_and_source_digests() {
        let block = render_readme_status(&sample_progress(), PROGRESS_BYTES, PLAN_BYTES);

        assert_eq!(
            block,
            "<!-- sitegen:status:start -->\n\
             <!-- Generated by `sitegen readme --repository .`; never edit this block by hand. -->\n\
             \n\
             | Generated status | Value |\n\
             | --- | --- |\n\
             | Working now | `camera-orbit` — Add clamped four-way camera orbit |\n\
             | Tasks | 2 done, 1 in progress, 1 future, 4 total |\n\
             | Challenges | 1 open, 2 resolved, 3 total |\n\
             | Reviewed plan tasks | 1 |\n\
             | `docs/progress.json` | sha256 `2da8132c318debfb` |\n\
             | `docs/implementation-plan.md` | sha256 `2d4cd172d74e4d1b` |\n\
             <!-- sitegen:status:end -->"
        );
    }

    #[test]
    fn a_project_with_nothing_in_progress_publishes_that_instead_of_a_task() {
        let mut progress = sample_progress();
        progress
            .tasks
            .retain(|task| task.status == ProgressStatus::Done);

        let block = render_readme_status(&progress, PROGRESS_BYTES, PLAN_BYTES);

        assert!(
            block.contains("| Working now | all planned work complete |"),
            "{block}"
        );
        assert!(
            block.contains("| Tasks | 2 done, 0 in progress, 0 future, 2 total |"),
            "{block}"
        );
    }

    /// The block is the README's promise that it still describes the sources.
    /// A change either source can make that the published counts do not move —
    /// a reworded summary, an added plan paragraph — must still make a stored
    /// block stale, or the promise is only about the counts.
    #[test]
    fn a_source_change_the_counts_do_not_show_still_makes_a_stored_block_stale() {
        let progress = sample_progress();
        let baseline = render_readme_status(&progress, PROGRESS_BYTES, PLAN_BYTES);

        // One trailing space: the same document, different stored bytes.
        let reformatted = render_readme_status(&progress, "{\"canonical\":\"bytes\"} ", PLAN_BYTES);
        // One added paragraph: the same declared plan tasks, different prose.
        let annotated = render_readme_status(
            &progress,
            PROGRESS_BYTES,
            "# Reviewed plan\n\n## Task 5: Add clamped four-way camera orbit\n\nA note.\n",
        );

        assert!(
            reformatted.contains("sha256 `19483733685a28a5`"),
            "{reformatted}"
        );
        assert!(
            annotated.contains("sha256 `d3fa800b0dbe02a3`"),
            "{annotated}"
        );
        assert!(check_readme_status(&stored(&baseline), &reformatted).is_err());
        assert!(check_readme_status(&stored(&baseline), &annotated).is_err());
        assert!(check_readme_status(&stored(&baseline), &baseline).is_ok());
    }

    /// Task titles are prose, and a `|` in prose closes a Markdown table cell.
    /// A title that could open a column of its own would publish a row nobody
    /// wrote into a block nobody is allowed to hand-edit.
    #[test]
    fn a_task_title_can_never_break_out_of_its_generated_table_cell() {
        let mut progress = sample_progress();
        progress
            .tasks
            .iter_mut()
            .find(|task| task.status == ProgressStatus::InProgress)
            .expect("the sample document has a current task")
            .title = "Ship | now\nand later".to_owned();

        let block = render_readme_status(&progress, PROGRESS_BYTES, PLAN_BYTES);

        assert!(
            block.contains("| Working now | `camera-orbit` — Ship \\| now and later |\n"),
            "{block}"
        );
        assert_eq!(
            block.lines().count(),
            render_readme_status(&sample_progress(), PROGRESS_BYTES, PLAN_BYTES)
                .lines()
                .count()
        );
    }

    #[test]
    fn a_backslash_before_a_pipe_cannot_break_out_of_a_generated_table_cell() {
        let mut progress = sample_progress();
        progress
            .tasks
            .iter_mut()
            .find(|task| task.status == ProgressStatus::InProgress)
            .expect("the sample document has a current task")
            .title = r"Ship \| now".to_owned();

        let block = render_readme_status(&progress, PROGRESS_BYTES, PLAN_BYTES);

        assert!(
            block.contains(
                r"| Working now | `camera-orbit` — Ship \\\| now |
"
            ),
            "{block}"
        );
    }

    // -----------------------------------------------------------------------
    // Reading and replacing the block
    // -----------------------------------------------------------------------

    #[test]
    fn regenerating_the_block_preserves_every_byte_outside_it() {
        let readme = format!(
            "# Title\n\nProse before.\n\n{README_STATUS_START}\nold\n{README_STATUS_END}\n\nProse after.\n"
        );

        let updated = replace_readme_status(
            &readme,
            &format!("{README_STATUS_START}\nnew\n{README_STATUS_END}"),
        )
        .expect("a well formed block should be replaceable");

        assert_eq!(
            updated,
            format!(
                "# Title\n\nProse before.\n\n{README_STATUS_START}\nnew\n{README_STATUS_END}\n\nProse after.\n"
            )
        );
    }

    #[test]
    fn the_block_a_readme_carries_is_read_back_with_its_delimiters() {
        let block = format!("{README_STATUS_START}\nbody\n{README_STATUS_END}");
        let readme = format!("before\n{block}\nafter\n");

        assert_eq!(readme_status_block(&readme), Ok(block.as_str()));
    }

    /// Every one of these is a README a mutating command must refuse rather
    /// than "repair": each has more than one, or fewer than one, plausible
    /// span, and choosing any of them rewrites bytes the generator does not
    /// own.
    #[test]
    fn a_missing_duplicated_or_inverted_block_is_refused_rather_than_repaired() {
        let cases: [(&str, String, ReadmeStatusError); 6] = [
            (
                "neither delimiter",
                "# Title\n\nProse only.\n".to_owned(),
                ReadmeStatusError::Missing,
            ),
            (
                "opened but never closed",
                format!("before\n{README_STATUS_START}\nbody\n"),
                ReadmeStatusError::MissingDelimiter {
                    delimiter: README_STATUS_END,
                },
            ),
            (
                "closed but never opened",
                format!("before\nbody\n{README_STATUS_END}\n"),
                ReadmeStatusError::MissingDelimiter {
                    delimiter: README_STATUS_START,
                },
            ),
            (
                "two openings",
                format!("{README_STATUS_START}\na\n{README_STATUS_START}\nb\n{README_STATUS_END}"),
                ReadmeStatusError::DuplicateDelimiter {
                    delimiter: README_STATUS_START,
                    count: 2,
                },
            ),
            (
                "two closings",
                format!("{README_STATUS_START}\na\n{README_STATUS_END}\nb\n{README_STATUS_END}"),
                ReadmeStatusError::DuplicateDelimiter {
                    delimiter: README_STATUS_END,
                    count: 2,
                },
            ),
            (
                "closed before it opens",
                format!("{README_STATUS_END}\nbody\n{README_STATUS_START}"),
                ReadmeStatusError::Inverted,
            ),
        ];

        for (case, readme, expected) in cases {
            assert_eq!(
                readme_status_block(&readme),
                Err(expected.clone()),
                "{case}"
            );
            assert_eq!(
                replace_readme_status(&readme, "anything"),
                Err(expected),
                "{case}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // `sitegen check` against this repository
    // -----------------------------------------------------------------------

    /// The gate has to pass for the checkout it is committed in, and its
    /// verdict may not depend on where it was launched from.
    #[test]
    fn check_passes_for_this_repository_from_an_unrelated_working_directory() {
        let finished = sitegen(
            &["check", "--repository", repository().to_str().unwrap()],
            &std::env::temp_dir(),
        );

        assert_eq!(
            finished.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&finished.stderr)
        );
        assert_eq!(String::from_utf8(finished.stdout).unwrap(), "ci-baseline\n");
        assert!(String::from_utf8_lossy(&finished.stderr).is_empty());
    }

    /// `check` builds a whole site to validate it. Leaving that build behind
    /// would grow the temporary directory of every machine and every runner
    /// that ever ran the gate.
    #[test]
    fn check_removes_the_site_it_builds_to_reach_its_verdict() {
        let child = Command::new(env!("CARGO_BIN_EXE_sitegen"))
            .current_dir(std::env::temp_dir())
            .args(["check", "--repository", repository().to_str().unwrap()])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("sitegen should launch");
        let prefix = format!("sitegen-check-{}-", child.id());
        let finished = child.wait_with_output().expect("sitegen should finish");

        assert_eq!(
            finished.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&finished.stderr)
        );
        let left_behind = fs::read_dir(std::env::temp_dir())
            .expect("the temporary directory should be readable")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(&prefix))
            .collect::<Vec<_>>();

        assert!(left_behind.is_empty(), "{left_behind:?}");
    }

    #[test]
    fn check_and_readme_require_a_repository() {
        for arguments in [
            vec!["check"],
            vec!["readme"],
            vec!["check", "--progress", "docs/progress.json"],
            vec!["readme", "--repository", ".", "--plan", "docs/x.md"],
        ] {
            let finished = sitegen(&arguments, repository());

            assert_eq!(finished.status.code(), Some(2), "{arguments:?}");
        }
    }

    // -----------------------------------------------------------------------
    // `sitegen check` against a mirror of this repository
    // -----------------------------------------------------------------------

    #[test]
    fn check_refuses_a_readme_whose_generated_block_is_missing_duplicated_or_stale() {
        for (case, mutate, expected) in [
            (
                "stale",
                &(|mirror: &Mirror| {
                    let readme = mirror.read("README.md");
                    let block = readme_status_block(&readme).unwrap().replace(
                        "| Reviewed plan tasks | 13 |",
                        "| Reviewed plan tasks | 99 |",
                    );
                    mirror.write(
                        "README.md",
                        &replace_readme_status(&readme, &block).unwrap(),
                    );
                }) as &dyn Fn(&Mirror),
                "the generated status block is stale",
            ),
            (
                "missing",
                &|mirror: &Mirror| {
                    let readme = mirror.read("README.md");
                    mirror.write("README.md", &replace_readme_status(&readme, "").unwrap());
                },
                "the generated status block is missing",
            ),
            (
                "duplicated",
                &|mirror: &Mirror| {
                    let readme = mirror.read("README.md");
                    let block = readme_status_block(&readme).unwrap().to_owned();
                    mirror.write("README.md", &format!("{readme}\n{block}\n"));
                },
                "appears 2 times",
            ),
            (
                "left behind by an edited progress document",
                &|mirror: &Mirror| {
                    let progress = mirror.read("docs/progress.json");
                    mirror.write("docs/progress.json", &format!("{progress}\n"));
                },
                "the generated status block is stale",
            ),
        ] {
            let mirror = Mirror::new(case);
            mutate(&mirror);
            let refused = mirror.read("README.md");

            let finished = mirror.run("check");
            let stderr = String::from_utf8_lossy(&finished.stderr).into_owned();

            assert_eq!(finished.status.code(), Some(1), "{case}: {stderr}");
            assert!(stderr.contains("README.md"), "{case}: {stderr}");
            assert!(stderr.contains(expected), "{case}: {stderr}");
            // The gate reports; only `readme` repairs. A check that quietly
            // rewrote the block would pass on its own second run and publish
            // whatever the sources happened to say.
            assert_eq!(mirror.read("README.md"), refused, "{case}");
        }
    }

    /// The reviewed plan declares task IDs through heading prose alone, so a
    /// reworded heading silently stops declaring them. The gate is what turns
    /// that silence into a failure.
    #[test]
    fn check_refuses_progress_task_ids_the_reviewed_plan_no_longer_declares() {
        let mirror = Mirror::new("plan-heading");
        let plan = mirror.read("docs/implementation-plan.md");
        mirror.write(
            "docs/implementation-plan.md",
            &plan.replace(
                "Task 9: Add CI and publish the reproducible POC baseline",
                "Task 9: Add CI and publish the POC baseline",
            ),
        );
        mirror.regenerate_readme();

        let finished = mirror.run("check");
        let stderr = String::from_utf8_lossy(&finished.stderr).into_owned();

        assert_eq!(finished.status.code(), Some(1), "{stderr}");
        assert!(
            stderr.contains("task id is not in the reviewed plan: ci-baseline"),
            "{stderr}"
        );
    }

    #[test]
    fn check_refuses_a_reference_asset_that_no_longer_matches_the_manifest() {
        let mirror = Mirror::new("reference-drift");
        let key_art = mirror.path().join("docs/reference/cel-shift-key-art.png");
        let mut bytes = fs::read(&key_art).unwrap();
        bytes.push(0);
        fs::write(&key_art, bytes).unwrap();

        let finished = mirror.run("check");
        let stderr = String::from_utf8_lossy(&finished.stderr).into_owned();

        assert_eq!(finished.status.code(), Some(1), "{stderr}");
        assert!(stderr.contains("cel-shift-key-art.png"), "{stderr}");
    }

    /// Nothing above this reaches the generator: progress, references, and the
    /// README are all judged from the sources alone. This is the failure only
    /// a real build finds — two reviewed headings declaring one task ID
    /// publish the same anchor twice — so it is the proof that `check` really
    /// generates and validates a site rather than only reading documents.
    #[test]
    fn check_refuses_a_plan_that_generates_a_page_the_site_rules_reject() {
        let mirror = Mirror::new("duplicate-anchor");
        let plan = mirror.read("docs/implementation-plan.md");
        mirror.write(
            "docs/implementation-plan.md",
            &format!("{plan}\n### Task 5: Add clamped four-way camera orbit\n"),
        );
        mirror.regenerate_readme();

        let finished = mirror.run("check");
        let stderr = String::from_utf8_lossy(&finished.stderr).into_owned();

        assert_eq!(finished.status.code(), Some(1), "{stderr}");
        assert!(stderr.contains("duplicate id"), "{stderr}");
        assert!(stderr.contains("plan-camera-orbit"), "{stderr}");
    }

    // -----------------------------------------------------------------------
    // `sitegen readme`
    // -----------------------------------------------------------------------

    #[test]
    fn regenerating_the_readme_rewrites_only_the_block_and_restores_the_gate() {
        let mirror = Mirror::new("regenerate");
        let before = mirror.read("README.md");
        let progress = mirror.read("docs/progress.json");
        mirror.write("docs/progress.json", &format!("{progress}\n"));
        assert_eq!(mirror.run("check").status.code(), Some(1));

        let regenerated = mirror.run("readme");
        assert_eq!(
            regenerated.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&regenerated.stderr)
        );

        let after = mirror.read("README.md");
        assert_ne!(after, before);
        assert_eq!(outside_block(&after), outside_block(&before));
        assert_eq!(
            mirror.run("check").status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&mirror.run("check").stderr)
        );
    }

    /// A README with two opening delimiters has no single span the generator
    /// owns. Rewriting one of them would silently rewrite bytes a person
    /// wrote, so the command refuses and leaves the file exactly as it found
    /// it.
    #[test]
    fn regenerating_a_malformed_readme_refuses_and_writes_nothing() {
        let mirror = Mirror::new("malformed-regenerate");
        let readme = mirror.read("README.md");
        let block = readme_status_block(&readme).unwrap().to_owned();
        let malformed = format!("{readme}\n{block}\n");
        mirror.write("README.md", &malformed);

        let finished = mirror.run("readme");
        let stderr = String::from_utf8_lossy(&finished.stderr).into_owned();

        assert_eq!(finished.status.code(), Some(1), "{stderr}");
        assert!(stderr.contains("appears 2 times"), "{stderr}");
        assert_eq!(mirror.read("README.md"), malformed);
        assert!(
            fs::read_dir(mirror.path())
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().ends_with(".tmp")),
            "a refused regeneration must leave no partial file behind"
        );
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Stored bytes that say nothing about the document beside them, so a
    /// digest row can only come from the bytes it was handed.
    const PROGRESS_BYTES: &str = "{\"canonical\":\"bytes\"}";

    /// A plan declaring exactly one reviewed task ID.
    const PLAN_BYTES: &str = "# Reviewed plan\n\n## Task 5: Add clamped four-way camera orbit\n";

    fn sample_progress() -> ProgressDocument {
        ProgressDocument {
            schema_version: 1,
            project: "Cell Shift Data Center POC".to_owned(),
            tasks: vec![
                task(
                    "foundation-contracts",
                    "Establish reviewed contracts",
                    ProgressStatus::Done,
                ),
                task(
                    "pages-foundation",
                    "Publish the status-only hub",
                    ProgressStatus::Done,
                ),
                task(
                    "camera-orbit",
                    "Add clamped four-way camera orbit",
                    ProgressStatus::InProgress,
                ),
                task(
                    "ci-baseline",
                    "Publish the reproducible baseline",
                    ProgressStatus::Future,
                ),
            ],
            challenges: vec![
                challenge("open-one", ChallengeStatus::Open),
                challenge("resolved-one", ChallengeStatus::Resolved),
                challenge("resolved-two", ChallengeStatus::Resolved),
            ],
        }
    }

    fn task(id: &str, title: &str, status: ProgressStatus) -> ProgressTask {
        ProgressTask {
            id: id.to_owned(),
            title: title.to_owned(),
            status,
            depends_on: Vec::new(),
            summary: "Summary.".to_owned(),
            completed_commit: (status == ProgressStatus::Done).then(|| "1".repeat(40)),
        }
    }

    fn challenge(id: &str, status: ChallengeStatus) -> Challenge {
        Challenge {
            id: id.to_owned(),
            title: "A challenge".to_owned(),
            status,
            impact: "Impact.".to_owned(),
            approach: "Approach.".to_owned(),
            resolution: (status == ChallengeStatus::Resolved).then(|| "Resolved.".to_owned()),
            resolved_commit: (status == ChallengeStatus::Resolved).then(|| "1".repeat(40)),
        }
    }

    /// A README that carries exactly one generated block.
    fn stored(block: &str) -> String {
        format!("# Title\n\n{block}\n\nProse.\n")
    }

    /// Everything a README says outside its generated block.
    fn outside_block(readme: &str) -> String {
        let block = readme_status_block(readme).expect("the README should carry one block");
        readme.replace(block, "")
    }

    fn repository() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    }

    fn sitegen(arguments: &[&str], working_directory: &Path) -> Output {
        Command::new(env!("CARGO_BIN_EXE_sitegen"))
            .current_dir(working_directory)
            .args(arguments)
            .output()
            .expect("sitegen should launch")
    }

    /// A second checkout of this repository holding only the documents
    /// `sitegen check` reads.
    ///
    /// The object database is shared with the real checkout, so every commit
    /// `docs/progress.json` names still resolves and `HEAD` is this branch's
    /// head. The documents themselves are copied from the working tree rather
    /// than checked out of `HEAD`, so a mirror judges the sources as they are
    /// right now and the only thing wrong with one is the thing a test put
    /// there.
    struct Mirror(PathBuf);

    impl Mirror {
        fn new(name: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = repository()
                .join("target/check-mirrors")
                .join(format!("{name}-{}-{unique}", std::process::id()));
            fs::create_dir_all(root.parent().unwrap()).unwrap();
            git(&[
                "clone",
                "--quiet",
                "--shared",
                "--no-checkout",
                repository().to_str().unwrap(),
                root.to_str().unwrap(),
            ]);
            copy_tree(&repository().join("docs"), &root.join("docs"));
            fs::copy(repository().join("README.md"), root.join("README.md")).unwrap();
            Self(root)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn read(&self, relative: &str) -> String {
            read(self.0.join(relative))
        }

        fn write(&self, relative: &str, contents: &str) {
            fs::write(self.0.join(relative), contents).unwrap();
        }

        fn run(&self, command: &str) -> Output {
            sitegen(
                &[command, "--repository", self.0.to_str().unwrap()],
                &std::env::temp_dir(),
            )
        }

        fn regenerate_readme(&self) {
            let finished = self.run("readme");
            assert_eq!(
                finished.status.code(),
                Some(0),
                "{}",
                String::from_utf8_lossy(&finished.stderr)
            );
        }
    }

    impl Drop for Mirror {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn git(arguments: &[&str]) {
        let finished = Command::new("git")
            .args(arguments)
            .output()
            .expect("git should launch");
        assert!(
            finished.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&finished.stderr)
        );
    }

    fn copy_tree(source: &Path, destination: &Path) {
        fs::create_dir_all(destination).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let target = destination.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }
}
