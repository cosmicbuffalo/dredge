use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::Result;
use serde_json::Value;

use crate::model::{Provider, TranscriptFile};
use crate::normalize::normalize_path;
use crate::paths::AppPaths;

pub fn discover(paths: &AppPaths) -> Result<Vec<TranscriptFile>> {
    let mut files = Vec::new();
    discover_claude(&paths.claude_root, &mut files);
    discover_codex(&paths.codex_root, &mut files);
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn discover_claude(root: &Path, files: &mut Vec<TranscriptFile>) {
    let projects = root.join("projects");
    for project in readable_dirs(&projects) {
        for path in readable_files(&project) {
            if path.extension().and_then(|part| part.to_str()) == Some("jsonl") {
                push_file(Provider::Claude, path, files);
            }
        }
    }
}

fn discover_codex(root: &Path, files: &mut Vec<TranscriptFile>) {
    let sessions = root.join("sessions");
    for year in readable_dirs(&sessions) {
        for month in readable_dirs(&year) {
            for day in readable_dirs(&month) {
                for path in readable_files(&day) {
                    let name = path
                        .file_name()
                        .and_then(|part| part.to_str())
                        .unwrap_or("");
                    if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                        push_file(Provider::Codex, path, files);
                    }
                }
            }
        }
    }
}

fn readable_dirs(path: &Path) -> Vec<PathBuf> {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(|kind| kind.is_dir())
                .map(|_| entry.path())
        })
        .collect()
}

fn readable_files(path: &Path) -> Vec<PathBuf> {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(|kind| kind.is_file())
                .map(|_| entry.path())
        })
        .collect()
}

fn push_file(provider: Provider, path: PathBuf, files: &mut Vec<TranscriptFile>) {
    let Ok(metadata) = fs::metadata(&path) else {
        return;
    };
    let mtime_ns = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    files.push(TranscriptFile {
        provider,
        path: normalize_path(&path),
        size: metadata.len(),
        mtime_ns,
    });
}

pub fn codex_thread_names(root: &Path) -> HashMap<String, String> {
    let mut names = HashMap::new();
    let Ok(contents) = fs::read_to_string(root.join("session_index.jsonl")) else {
        return names;
    };
    for line in contents.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let id = string_at(&value, &["id"])
            .or_else(|| string_at(&value, &["session_id"]))
            .or_else(|| string_at(&value, &["thread_id"]));
        let title = string_at(&value, &["thread_name"])
            .or_else(|| string_at(&value, &["name"]))
            .map(crate::normalize::bounded_display_text);
        if let (Some(id), Some(title)) = (id, title)
            && !title.is_empty()
        {
            names.insert(id.to_owned(), title);
        }
    }
    names
}

fn string_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter()
        .try_fold(value, |current, key| current.get(key))?
        .as_str()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn discovery_excludes_nested_claude_agents_and_wrong_codex_layouts() {
        let temp = tempdir().unwrap();
        let claude = temp.path().join("claude");
        let codex = temp.path().join("codex");
        fs::create_dir_all(claude.join("projects/p/agents")).unwrap();
        fs::write(claude.join("projects/p/session.jsonl"), "").unwrap();
        fs::write(claude.join("projects/p/agents/sub.jsonl"), "").unwrap();
        fs::create_dir_all(codex.join("sessions/2026/01/02")).unwrap();
        fs::write(codex.join("sessions/2026/01/02/rollout-a.jsonl"), "").unwrap();
        fs::write(codex.join("sessions/2026/01/02/other.jsonl"), "").unwrap();
        let paths = AppPaths {
            claude_root: claude,
            codex_root: codex,
            data_root: temp.path().join("data"),
        };
        let found = discover(&paths).unwrap();
        assert_eq!(found.len(), 2);
    }
}
