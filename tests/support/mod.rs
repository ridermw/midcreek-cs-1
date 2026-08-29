use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use midcreek_cs_1::sitegen::{ProgressDocument, ProgressError, RepoFacts, validate_progress};
use sha2::{Digest, Sha256};

pub fn read(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path.as_ref()).expect("fixture should be readable")
}

pub fn sha256(path: impl AsRef<Path>) -> String {
    let bytes = fs::read(path.as_ref()).expect("file should be readable");
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn fixture(name: &str) -> ProgressDocument {
    let path = fixture_root().join(name);
    serde_json::from_str(&read(path)).expect("fixture should match the progress schema")
}

pub fn validate_fixture(name: &str) -> Vec<ProgressError> {
    validate_progress(&fixture(name), &plan_ids(), &repo_facts())
        .expect_err("fixture should fail validation")
}

pub fn plan_ids() -> BTreeSet<String> {
    ["foundation-contracts", "pages-foundation"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

pub fn repo_facts() -> RepoFacts {
    let head_sha = "1111111111111111111111111111111111111111".to_owned();
    RepoFacts {
        known_commits: BTreeSet::from([
            head_sha.clone(),
            "2222222222222222222222222222222222222222".to_owned(),
        ]),
        head_sha,
        commits: Vec::new(),
    }
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sitegen")
}
