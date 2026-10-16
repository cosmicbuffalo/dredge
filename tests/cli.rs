use std::fs;
use std::process::Command;

use serde_json::{Value, json};
use tempfile::TempDir;

struct CliFixture {
    temp: TempDir,
}

impl CliFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("claude/projects/example")).unwrap();
        let transcript = [
            json!({
                "type": "custom-title",
                "customTitle": "Release planning"
            }),
            json!({
                "type": "user",
                "timestamp": "2026-10-15T10:00:00Z",
                "cwd": temp.path().join("work/project"),
                "gitBranch": "main",
                "message": {"role": "user", "content": "Review the release checklist"}
            }),
            json!({
                "type": "assistant",
                "timestamp": "2026-10-15T10:01:00Z",
                "message": {"role": "assistant", "content": "The package checklist is ready."}
            }),
        ];
        fs::write(
            temp.path().join("claude/projects/example/session-1.jsonl"),
            format!(
                "{}\n",
                transcript
                    .iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        )
        .unwrap();
        Self { temp }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_dredge"));
        command
            .env("HOME", self.temp.path())
            .env("CLAUDE_CONFIG_DIR", self.temp.path().join("claude"))
            .env("CODEX_HOME", self.temp.path().join("codex"))
            .env("DREDGE_DATA_DIR", self.temp.path().join("data"));
        command
    }
}

#[test]
fn json_commands_emit_one_versioned_object() {
    let fixture = CliFixture::new();
    let index = fixture
        .command()
        .args(["index", "--json"])
        .output()
        .unwrap();
    assert!(index.status.success());
    assert!(index.stderr.is_empty());
    let index_json: Value = serde_json::from_slice(&index.stdout).unwrap();
    assert_eq!(index_json["api_version"], 1);
    assert_eq!(index_json["index"]["sessions"], 1);
    #[cfg(unix)]
    assert_private_tree(&fixture.temp.path().join("data"));

    let search = fixture
        .command()
        .args(["search", "--refresh", "never", "--json", "checklist"])
        .output()
        .unwrap();
    assert!(search.status.success());
    assert!(search.stderr.is_empty());
    let search_json: Value = serde_json::from_slice(&search.stdout).unwrap();
    assert_eq!(search_json["api_version"], 1);
    assert_eq!(search_json["query"], "checklist");
    assert_eq!(search_json["index_refreshed"], false);
    assert_eq!(search_json["results"][0]["provider"], "claude");
    assert_eq!(search_json["results"][0]["session_id"], "session-1");
    assert!(search_json["results"][0]["score"].as_f64().unwrap() <= 1.0);

    let list = fixture
        .command()
        .args(["list", "--refresh", "never", "--json"])
        .output()
        .unwrap();
    let list_json: Value = serde_json::from_slice(&list.stdout).unwrap();
    assert!(list_json["results"][0]["score"].is_null());
    assert!(list_json["results"][0]["match_source"].is_null());
    assert!(list_json["results"][0]["snippet"].is_null());
}

#[cfg(unix)]
fn assert_private_tree(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::metadata(path).unwrap();
    assert_eq!(
        metadata.permissions().mode() & 0o077,
        0,
        "{}",
        path.display()
    );
    if metadata.is_dir() {
        for entry in fs::read_dir(path).unwrap() {
            assert_private_tree(&entry.unwrap().path());
        }
    }
}

#[test]
fn usage_errors_exit_two_and_completions_are_generated() {
    let fixture = CliFixture::new();
    let usage = fixture.command().arg("search").output().unwrap();
    assert_eq!(usage.status.code(), Some(2));
    let completions = fixture
        .command()
        .args(["completions", "bash"])
        .output()
        .unwrap();
    assert!(completions.status.success());
    assert!(!completions.stdout.is_empty());
}
