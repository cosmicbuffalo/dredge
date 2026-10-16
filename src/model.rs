use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const API_VERSION: u32 = 1;
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Claude,
    Codex,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug)]
pub struct TranscriptFile {
    pub provider: Provider,
    pub path: PathBuf,
    pub size: u64,
    pub mtime_ns: u128,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub provider: Provider,
    pub session_id: String,
    pub title: Option<String>,
    pub title_source: Option<String>,
    pub first_prompt: Option<String>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub started_at: Option<i64>,
    pub updated_at: i64,
    pub transcript_path: String,
    #[serde(skip)]
    pub conversation: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResultRecord {
    pub provider: Provider,
    pub session_id: String,
    pub title: Option<String>,
    pub title_source: Option<String>,
    pub first_prompt: Option<String>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub started_at: Option<i64>,
    pub updated_at: i64,
    pub transcript_path: String,
    pub score: Option<f32>,
    pub match_source: Option<String>,
    pub snippet: Option<String>,
}

impl ResultRecord {
    pub fn from_session(session: Session) -> Self {
        Self {
            provider: session.provider,
            session_id: session.session_id,
            title: session.title,
            title_source: session.title_source,
            first_prompt: session.first_prompt,
            cwd: session.cwd,
            git_branch: session.git_branch,
            started_at: session.started_at,
            updated_at: session.updated_at,
            transcript_path: session.transcript_path,
            score: None,
            match_source: None,
            snippet: None,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SearchEnvelope {
    pub api_version: u32,
    pub generated_at: i64,
    pub query: String,
    pub index_refreshed: bool,
    pub results: Vec<ResultRecord>,
}

#[derive(Debug, Serialize)]
pub struct ListEnvelope {
    pub api_version: u32,
    pub generated_at: i64,
    pub index_refreshed: bool,
    pub results: Vec<ResultRecord>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct IndexStats {
    pub schema_version: u32,
    pub sessions: usize,
    pub transcript_bytes: u64,
    pub changed: usize,
    pub removed: usize,
    pub failed: usize,
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct IndexEnvelope {
    pub api_version: u32,
    pub generated_at: i64,
    pub index: IndexStats,
}
