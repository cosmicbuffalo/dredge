use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::model::{Provider, Session, TranscriptFile};
use crate::normalize::{
    bounded_display_text, clean_text, normalize_path, parse_timestamp, terminal_safe,
};

const MAX_CONVERSATION_BYTES: usize = 16 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

pub fn parse(
    file: &TranscriptFile,
    codex_titles: &HashMap<String, String>,
) -> Result<Option<Session>> {
    match file.provider {
        Provider::Claude => parse_claude(file),
        Provider::Codex => parse_codex(file, codex_titles),
    }
}

#[derive(Default)]
struct Accumulator {
    first_prompt: Option<String>,
    conversation: String,
    started_at: Option<i64>,
    updated_at: Option<i64>,
    cwd: Option<String>,
    git_branch: Option<String>,
}

impl Accumulator {
    fn add_message(&mut self, role: &str, text: String) {
        if text.is_empty() || self.conversation.len() >= MAX_CONVERSATION_BYTES {
            return;
        }
        if role == "user" && self.first_prompt.is_none() {
            self.first_prompt = Some(bounded_display_text(&text));
        }
        let remaining = MAX_CONVERSATION_BYTES.saturating_sub(self.conversation.len());
        if !self.conversation.is_empty() && remaining > 0 {
            self.conversation.push('\n');
        }
        let remaining = MAX_CONVERSATION_BYTES.saturating_sub(self.conversation.len());
        let end = text
            .char_indices()
            .map(|(index, _)| index)
            .take_while(|index| *index <= remaining)
            .last()
            .unwrap_or(0);
        if text.len() <= remaining {
            self.conversation.push_str(&text);
        } else {
            self.conversation.push_str(&text[..end]);
        }
    }

    fn observe_timestamp(&mut self, timestamp: Option<i64>) {
        if let Some(timestamp) = timestamp {
            self.started_at = Some(self.started_at.map_or(timestamp, |old| old.min(timestamp)));
            self.updated_at = Some(self.updated_at.map_or(timestamp, |old| old.max(timestamp)));
        }
    }
}

fn parse_claude(file: &TranscriptFile) -> Result<Option<Session>> {
    let mut accumulator = Accumulator::default();
    let mut custom_title = None;
    let mut ai_title = None;
    let mut legacy_title = None;
    let mut excluded = false;
    for_each_json_line(file, |value| {
        let record_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        if value.get("isSidechain").and_then(Value::as_bool) == Some(true)
            || value.get("agentId").is_some()
            || is_noninteractive_source(&value)
        {
            excluded = true;
        }
        if let Some(cwd) = text_value(&value, &["cwd"]) {
            accumulator.cwd = Some(cwd.to_owned());
        }
        if let Some(branch) = text_value(&value, &["gitBranch"]) {
            accumulator.git_branch = Some(branch.to_owned());
        }
        accumulator.observe_timestamp(
            value
                .get("timestamp")
                .and_then(parse_timestamp)
                .or_else(|| value.get("created_at").and_then(parse_timestamp)),
        );
        match record_type {
            "custom-title" | "custom_title" => {
                custom_title = first_text(&value, &["customTitle", "title", "name"])
            }
            "ai-title" | "ai_title" => ai_title = first_text(&value, &["aiTitle", "title", "name"]),
            "summary" | "title" => legacy_title = first_text(&value, &["summary", "title"]),
            "user" | "assistant" => {
                let role = value
                    .pointer("/message/role")
                    .and_then(Value::as_str)
                    .unwrap_or(record_type);
                if matches!(role, "user" | "assistant") {
                    let content = value.pointer("/message/content");
                    let text = extract_content(content, role == "user");
                    if !text.is_empty() {
                        accumulator.add_message(role, text);
                    }
                }
            }
            _ => {}
        }
    })?;
    if excluded || accumulator.first_prompt.is_none() {
        return Ok(None);
    }
    let session_id = file
        .path
        .file_stem()
        .and_then(|part| part.to_str())
        .unwrap_or_default()
        .to_owned();
    let (title, title_source) = choose_title(
        custom_title.map(|title| (title, "custom")),
        ai_title.map(|title| (title, "ai")),
        legacy_title.map(|title| (title, "ai")),
        accumulator.first_prompt.clone(),
    );
    Ok(Some(finish_session(
        Provider::Claude,
        session_id,
        title,
        title_source,
        accumulator,
        file,
    )))
}

fn parse_codex(
    file: &TranscriptFile,
    thread_names: &HashMap<String, String>,
) -> Result<Option<Session>> {
    let mut accumulator = Accumulator::default();
    let mut session_id = None;
    let mut excluded = false;
    for_each_json_line(file, |value| {
        let record_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        let payload = value.get("payload").unwrap_or(&value);
        accumulator.observe_timestamp(
            value
                .get("timestamp")
                .and_then(parse_timestamp)
                .or_else(|| payload.get("timestamp").and_then(parse_timestamp)),
        );
        if record_type == "session_meta" {
            if let Some(id) =
                text_value(payload, &["id"]).or_else(|| text_value(payload, &["session_id"]))
            {
                session_id = Some(id.to_owned());
            }
            if let Some(cwd) = text_value(payload, &["cwd"]) {
                accumulator.cwd = Some(cwd.to_owned());
            }
            if let Some(branch) = text_value(payload, &["git", "branch"])
                .or_else(|| text_value(payload, &["git_branch"]))
            {
                accumulator.git_branch = Some(branch.to_owned());
            }
            excluded |= codex_source_excluded(payload.get("source"));
            excluded |= payload
                .get("thread_source")
                .and_then(Value::as_str)
                .is_some_and(|source| source != "user");
        } else if record_type == "response_item"
            && payload.get("type").and_then(Value::as_str) == Some("message")
        {
            let role = payload.get("role").and_then(Value::as_str).unwrap_or("");
            if matches!(role, "user" | "assistant") {
                let text = extract_content(payload.get("content"), role == "user");
                if !text.is_empty() {
                    accumulator.add_message(role, text);
                }
            }
        }
    })?;
    if excluded || accumulator.first_prompt.is_none() {
        return Ok(None);
    }
    let session_id = session_id.unwrap_or_else(|| {
        let stem = file
            .path
            .file_stem()
            .and_then(|part| part.to_str())
            .unwrap_or_default();
        stem.char_indices()
            .rev()
            .nth(35)
            .map(|(start, _)| stem[start..].to_owned())
            .unwrap_or_else(|| stem.trim_start_matches("rollout-").to_owned())
    });
    let index_title = thread_names.get(&session_id).cloned();
    let (title, title_source) = choose_title(
        None,
        index_title.map(|title| (title, "index")),
        None,
        accumulator.first_prompt.clone(),
    );
    Ok(Some(finish_session(
        Provider::Codex,
        session_id,
        title,
        title_source,
        accumulator,
        file,
    )))
}

fn for_each_json_line(file: &TranscriptFile, mut consume: impl FnMut(Value)) -> Result<()> {
    let input = File::open(&file.path)
        .with_context(|| format!("open transcript {}", file.path.display()))?;
    let mut reader = BufReader::new(input);
    let mut line = Vec::new();
    while let Some(within_limit) = read_bounded_line(&mut reader, &mut line)? {
        if within_limit && let Ok(value) = serde_json::from_slice(&line) {
            consume(value);
        }
    }
    Ok(())
}

fn read_bounded_line(reader: &mut impl BufRead, line: &mut Vec<u8>) -> Result<Option<bool>> {
    line.clear();
    let mut within_limit = true;
    let mut saw_bytes = false;
    loop {
        let (consumed, ended) = {
            let available = reader.fill_buf().context("read transcript line")?;
            if available.is_empty() {
                return Ok(saw_bytes.then_some(within_limit));
            }
            saw_bytes = true;
            let consumed = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |position| position + 1);
            if within_limit {
                if line.len().saturating_add(consumed) <= MAX_LINE_BYTES {
                    line.extend_from_slice(&available[..consumed]);
                } else {
                    within_limit = false;
                    line.clear();
                }
            }
            (consumed, available.get(consumed - 1) == Some(&b'\n'))
        };
        reader.consume(consumed);
        if ended {
            return Ok(Some(within_limit));
        }
    }
}

fn finish_session(
    provider: Provider,
    session_id: String,
    title: Option<String>,
    title_source: Option<String>,
    accumulator: Accumulator,
    file: &TranscriptFile,
) -> Session {
    let fallback_time = i64::try_from(file.mtime_ns / 1_000_000_000).unwrap_or(i64::MAX);
    Session {
        provider,
        session_id: terminal_safe(&session_id),
        title: title.map(|title| bounded_display_text(&title)),
        title_source,
        first_prompt: accumulator
            .first_prompt
            .map(|prompt| bounded_display_text(&prompt)),
        cwd: accumulator.cwd.map(|cwd| {
            terminal_safe(&normalize_path(std::path::Path::new(&cwd)).to_string_lossy())
        }),
        git_branch: accumulator.git_branch.map(|branch| clean_text(&branch)),
        started_at: accumulator.started_at,
        updated_at: accumulator.updated_at.unwrap_or(fallback_time),
        transcript_path: terminal_safe(&file.path.to_string_lossy()),
        conversation: accumulator.conversation,
    }
}

fn choose_title(
    custom: Option<(String, &'static str)>,
    generated: Option<(String, &'static str)>,
    legacy: Option<(String, &'static str)>,
    prompt: Option<String>,
) -> (Option<String>, Option<String>) {
    custom
        .or(generated)
        .or(legacy)
        .or_else(|| prompt.map(|title| (title, "prompt")))
        .map(|(title, source)| (Some(bounded_display_text(&title)), Some(source.to_owned())))
        .unwrap_or((None, None))
}

fn first_text(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .map(bounded_display_text)
        .filter(|text| !text.is_empty())
}

fn text_value<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter()
        .try_fold(value, |current, key| current.get(key))?
        .as_str()
}

fn is_noninteractive_source(value: &Value) -> bool {
    let source = value
        .get("entrypoint")
        .or_else(|| value.get("source"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    source.starts_with("sdk")
        || matches!(
            source.as_str(),
            "non-interactive" | "noninteractive" | "print"
        )
}

fn codex_source_excluded(source: Option<&Value>) -> bool {
    match source {
        Some(Value::String(source)) => {
            matches!(
                source.to_ascii_lowercase().as_str(),
                "exec" | "subagent" | "spawned"
            )
        }
        Some(Value::Object(source)) => source.keys().any(|key| {
            matches!(
                key.to_ascii_lowercase().as_str(),
                "exec" | "subagent" | "spawned"
            )
        }),
        _ => false,
    }
}

fn extract_content(content: Option<&Value>, user: bool) -> String {
    let Some(content) = content else {
        return String::new();
    };
    let mut parts = Vec::new();
    match content {
        Value::String(text) => {
            if !user || !injected_user_text(text) {
                parts.push(text.as_str());
            }
        }
        Value::Array(blocks) => {
            for block in blocks {
                let kind = block.get("type").and_then(Value::as_str).unwrap_or("text");
                if !matches!(kind, "text" | "input_text" | "output_text") {
                    continue;
                }
                if let Some(text) = block.get("text").and_then(Value::as_str)
                    && (!user || !injected_user_text(text))
                {
                    parts.push(text);
                }
            }
        }
        _ => {}
    }
    clean_text(&parts.join(" "))
}

fn injected_user_text(text: &str) -> bool {
    let trimmed = text.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("<system-reminder")
        || lower.starts_with("<command-name>")
        || lower.starts_with("<command-message>")
        || lower.starts_with("<command-args>")
        || lower.starts_with("<command-caveat>")
        || lower.starts_with("<local-command")
        || lower.starts_with("<memory>")
        || lower.starts_with("# agents.md instructions")
        || lower.starts_with("# claude.md instructions")
        || lower.starts_with("<environment_context>")
        || lower.starts_with("<user_instructions>")
        || (lower.contains("<instructions>")
            && (lower.contains("agents.md") || lower.contains("claude.md")))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn file(path: &std::path::Path, provider: Provider) -> TranscriptFile {
        TranscriptFile {
            provider,
            path: path.to_path_buf(),
            size: fs::metadata(path).unwrap().len(),
            mtime_ns: 1_700_000_000_000_000_000,
        }
    }

    #[test]
    fn parses_claude_and_discards_wrappers_and_tools() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("abc.jsonl");
        fs::write(
            &path,
            concat!(
                "{\"type\":\"user\",\"cwd\":\"/work\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"<system-reminder>secret needle</system-reminder>\"},{\"type\":\"text\",\"text\":\"Fix the parser\"}]}}\n",
                "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"thinking\",\"thinking\":\"hidden\"},{\"type\":\"text\",\"text\":\"Done now\"},{\"type\":\"tool_use\",\"name\":\"Read\"}]}}\n",
                "{bad final"
            ),
        )
        .unwrap();
        let session = parse(&file(&path, Provider::Claude), &HashMap::new())
            .unwrap()
            .unwrap();
        assert_eq!(session.session_id, "abc");
        assert_eq!(session.first_prompt.as_deref(), Some("Fix the parser"));
        assert!(session.conversation.contains("Done now"));
        assert!(!session.conversation.contains("needle"));
        assert!(!session.conversation.contains("hidden"));
    }

    #[test]
    fn caps_multibyte_prompt_and_provider_title_but_keeps_full_conversation() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("long.jsonl");
        let title = format!("{}title-tail", "🦀".repeat(240));
        let prompt = format!("{}prompt-tail", "界".repeat(240));
        let transcript = [
            serde_json::json!({"type": "custom-title", "customTitle": title}),
            serde_json::json!({
                "type": "user",
                "message": {"role": "user", "content": prompt}
            }),
        ];
        fs::write(
            &path,
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

        let session = parse(&file(&path, Provider::Claude), &HashMap::new())
            .unwrap()
            .unwrap();
        assert_eq!(
            session.title.as_deref(),
            Some(format!("{}…", "🦀".repeat(240)).as_str())
        );
        assert_eq!(
            session.first_prompt.as_deref(),
            Some(format!("{}…", "界".repeat(240)).as_str())
        );
        assert!(session.conversation.ends_with("prompt-tail"));
        assert!(session.conversation.chars().count() > 240);
    }

    #[test]
    fn parses_codex_metadata_and_response_messages() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("rollout-x.jsonl");
        fs::write(
            &path,
            concat!(
                "{\"timestamp\":\"2026-02-01T00:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"thread-1\",\"cwd\":\"/work\",\"source\":\"cli\",\"git\":{\"branch\":\"main\"}}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"Ship it\"}]}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"ignore me\"}}\n"
            ),
        )
        .unwrap();
        let mut titles = HashMap::new();
        titles.insert("thread-1".to_owned(), "Release work".to_owned());
        let session = parse(&file(&path, Provider::Codex), &titles)
            .unwrap()
            .unwrap();
        assert_eq!(session.title.as_deref(), Some("Release work"));
        assert_eq!(session.title_source.as_deref(), Some("index"));
        assert!(!session.conversation.contains("ignore me"));
    }

    #[test]
    fn excludes_codex_exec_sessions() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("rollout-x.jsonl");
        fs::write(
            &path,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"x\",\"source\":\"exec\"}}\n{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":\"hello\"}}\n",
        )
        .unwrap();
        assert!(
            parse(&file(&path, Provider::Codex), &HashMap::new())
                .unwrap()
                .is_none()
        );
    }
}
