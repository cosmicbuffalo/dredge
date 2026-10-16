use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use fs2::FileExt;
use levenshtein_automata::{DFA, Distance, LevenshteinAutomatonBuilder};
use serde::{Deserialize, Serialize};
use tantivy::collector::TopDocs;
use tantivy::query::{AllQuery, BooleanQuery, BoostQuery, Occur, PhraseQuery, Query, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions, Value,
};
use tantivy::tokenizer::{LowerCaser, RemoveLongFilter, SimpleTokenizer, TextAnalyzer};
use tantivy::{Index, IndexReader, ReloadPolicy, TantivyDocument, Term, doc};
use tantivy_fst::Automaton;

use crate::discovery::{codex_thread_names, discover};
use crate::model::{IndexStats, Provider, ResultRecord, SCHEMA_VERSION, Session, TranscriptFile};
use crate::normalize::{clean_text, normalize_path, now, query_tokens};
use crate::parser;
use crate::paths::{AppPaths, set_mode};
use crate::ranking;

const WRITER_HEAP_BYTES: usize = 50_000_000;
const MAX_QUERY_TOKEN_BYTES: usize = 64;
const TOKENIZER_NAME: &str = "dredge";
const MAX_FUZZY_EXPANSIONS: usize = 256;
const MAX_EXPANSIONS_PER_CLAUSE: usize = 32;
const CONTENT_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Manifest {
    schema_version: u32,
    #[serde(default)]
    content_version: u32,
    files: BTreeMap<String, ManifestEntry>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            content_version: CONTENT_VERSION,
            files: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ManifestEntry {
    provider: Provider,
    path: String,
    size: u64,
    mtime_ns: u128,
    status: ParseStatus,
    session_id: Option<String>,
    index_title: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum ParseStatus {
    Indexed,
    Skipped,
    Failed,
}

#[derive(Clone, Copy, Debug)]
pub enum ProviderFilter {
    Claude,
    Codex,
}

impl ProviderFilter {
    fn matches(self, provider: Provider) -> bool {
        matches!(
            (self, provider),
            (Self::Claude, Provider::Claude) | (Self::Codex, Provider::Codex)
        )
    }
}

#[derive(Debug)]
pub struct RefreshOutcome {
    pub stats: IndexStats,
    pub refreshed: bool,
}

#[derive(Clone)]
struct Fields {
    key: Field,
    provider: Field,
    session_id: Field,
    title: Field,
    title_source: Field,
    first_prompt: Field,
    cwd: Field,
    git_branch: Field,
    started_at: Field,
    updated_at: Field,
    transcript_path: Field,
    conversation: Field,
}

impl Fields {
    fn from_schema(schema: &Schema) -> Result<Self> {
        let field = |name| {
            schema
                .get_field(name)
                .map_err(|_| anyhow!("index schema is missing field {name}"))
        };
        Ok(Self {
            key: field("key")?,
            provider: field("provider")?,
            session_id: field("session_id")?,
            title: field("title")?,
            title_source: field("title_source")?,
            first_prompt: field("first_prompt")?,
            cwd: field("cwd")?,
            git_branch: field("git_branch")?,
            started_at: field("started_at")?,
            updated_at: field("updated_at")?,
            transcript_path: field("transcript_path")?,
            conversation: field("conversation")?,
        })
    }
}

pub struct Engine {
    paths: AppPaths,
    index: Index,
    fields: Fields,
}

impl Engine {
    pub fn open(paths: AppPaths) -> Result<Self> {
        paths.ensure_safe_data_root()?;
        let _lock = RefreshLock::acquire(&paths)?;
        let (index, rebuilt) = open_index(&paths)?;
        index.tokenizers().register(
            TOKENIZER_NAME,
            TextAnalyzer::builder(SimpleTokenizer::default())
                .filter(RemoveLongFilter::limit(MAX_QUERY_TOKEN_BYTES + 1))
                .filter(LowerCaser)
                .build(),
        );
        let fields = Fields::from_schema(&index.schema())?;
        if rebuilt && paths.manifest_path().exists() {
            fs::remove_file(paths.manifest_path()).context("discard incompatible manifest")?;
        }
        Ok(Self {
            paths,
            index,
            fields,
        })
    }

    pub fn refresh(&self, force: bool) -> Result<RefreshOutcome> {
        let _lock = RefreshLock::acquire(&self.paths)?;
        let manifest_path = self.paths.manifest_path();
        let manifest_was_invalid = manifest_path.exists() && load_manifest(&manifest_path).is_err();
        let mut manifest = load_manifest(&manifest_path).unwrap_or_default();
        let mut reset_index = manifest_was_invalid;
        if manifest.schema_version != SCHEMA_VERSION || manifest.content_version != CONTENT_VERSION
        {
            manifest = Manifest::default();
            reset_index = true;
        }
        let files = discover(&self.paths)?;
        let codex_titles = codex_thread_names(&self.paths.codex_root);
        let discovered_keys: HashSet<String> = files.iter().map(file_key).collect();
        let removed_keys: Vec<String> = manifest
            .files
            .keys()
            .filter(|key| !discovered_keys.contains(*key))
            .cloned()
            .collect();

        let mut writer = self
            .index
            .writer(WRITER_HEAP_BYTES)
            .context("open index writer")?;
        if force || reset_index {
            writer.delete_all_documents().context("clear index")?;
            manifest = Manifest::default();
        }
        for key in &removed_keys {
            writer.delete_term(Term::from_field_text(self.fields.key, key));
            manifest.files.remove(key);
        }

        let mut changed = 0;
        let mut failed = 0;
        for file in files {
            let key = file_key(&file);
            let current_title = manifest
                .files
                .get(&key)
                .and_then(|entry| entry.session_id.as_ref())
                .and_then(|id| codex_titles.get(id))
                .cloned();
            let unchanged = !force
                && manifest.files.get(&key).is_some_and(|entry| {
                    entry.size == file.size
                        && entry.mtime_ns == file.mtime_ns
                        && (file.provider != Provider::Codex || entry.index_title == current_title)
                });
            if unchanged {
                continue;
            }
            changed += 1;
            writer.delete_term(Term::from_field_text(self.fields.key, &key));
            let (status, session_id, index_title) = match parser::parse(&file, &codex_titles) {
                Ok(Some(session)) => {
                    let session_id = Some(session.session_id.clone());
                    let index_title = if file.provider == Provider::Codex {
                        session_id
                            .as_ref()
                            .and_then(|id| codex_titles.get(id))
                            .cloned()
                    } else {
                        None
                    };
                    writer
                        .add_document(self.session_document(&key, &session))
                        .context("add session to index")?;
                    (ParseStatus::Indexed, session_id, index_title)
                }
                Ok(None) => (ParseStatus::Skipped, None, None),
                Err(error) => {
                    failed += 1;
                    eprintln!("dredge: could not index {}: {error:#}", file.path.display());
                    (ParseStatus::Failed, None, None)
                }
            };
            manifest.files.insert(
                key,
                ManifestEntry {
                    provider: file.provider,
                    path: file.path.to_string_lossy().into_owned(),
                    size: file.size,
                    mtime_ns: file.mtime_ns,
                    status,
                    session_id,
                    index_title,
                },
            );
        }
        let refreshed = changed > 0 || !removed_keys.is_empty() || force || reset_index;
        if refreshed {
            writer.commit().context("commit search index")?;
            secure_index_tree(&self.paths.index_dir())?;
            write_manifest(&self.paths.manifest_path(), &manifest)?;
        }
        let stats =
            stats_from_manifest(&manifest, &self.paths, changed, removed_keys.len(), failed);
        Ok(RefreshOutcome { stats, refreshed })
    }

    pub fn status(&self) -> Result<IndexStats> {
        let manifest = if self.paths.manifest_path().exists() {
            load_manifest(&self.paths.manifest_path())?
        } else {
            Manifest::default()
        };
        Ok(stats_from_manifest(&manifest, &self.paths, 0, 0, 0))
    }

    pub fn list(
        &self,
        limit: usize,
        provider: Option<ProviderFilter>,
        within: Option<&Path>,
    ) -> Result<Vec<ResultRecord>> {
        let reader = self.reader()?;
        let searcher = reader.searcher();
        let count = searcher.num_docs() as usize;
        let docs = searcher
            .search(&AllQuery, &TopDocs::with_limit(count.max(1)))
            .context("read sessions from index")?;
        let within = within.map(normalize_path);
        let mut results = Vec::new();
        for (_, address) in docs {
            let document: TantivyDocument =
                searcher.doc(address).context("read indexed session")?;
            let session = self.document_session(&document)?;
            if filtered(&session, provider, within.as_deref()) {
                continue;
            }
            results.push(ResultRecord::from_session(session));
        }
        results.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.provider.as_str().cmp(right.provider.as_str()))
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        truncate(&mut results, limit);
        Ok(results)
    }

    pub fn search(
        &self,
        query: &str,
        limit: usize,
        provider: Option<ProviderFilter>,
        within: Option<&Path>,
    ) -> Result<Vec<ResultRecord>> {
        let tokens: Vec<String> = query_tokens(query)
            .into_iter()
            .filter(|token| token.len() <= MAX_QUERY_TOKEN_BYTES)
            .collect();
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let reader = self.reader()?;
        let searcher = reader.searcher();
        let search_query = self.build_query(&searcher, &tokens)?;
        let count = searcher.num_docs() as usize;
        let docs = searcher
            .search(search_query.as_ref(), &TopDocs::with_limit(count.max(1)))
            .context("search index")?;
        let within = within.map(normalize_path);
        let max_raw = docs
            .iter()
            .map(|(score, _)| *score)
            .filter(|score| score.is_finite())
            .fold(0.0_f32, f32::max);
        let current_time = now();
        let mut results = Vec::new();
        for (raw_score, address) in docs {
            let document: TantivyDocument = searcher.doc(address).context("read search result")?;
            let session = self.document_session(&document)?;
            if filtered(&session, provider, within.as_deref()) {
                continue;
            }
            let relevance = if max_raw > 0.0 && raw_score.is_finite() {
                (raw_score / max_raw).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let score = ranking::combine(
                relevance,
                ranking::recency(current_time, session.updated_at),
            );
            let (source, snippet) = best_snippet(&session, &tokens);
            let mut result = ResultRecord::from_session(session);
            result.score = Some(score);
            result.match_source = Some(source.to_owned());
            result.snippet = Some(snippet);
            results.push(result);
        }
        results.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| right.updated_at.cmp(&left.updated_at))
                .then_with(|| left.provider.as_str().cmp(right.provider.as_str()))
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        truncate(&mut results, limit);
        Ok(results)
    }

    fn reader(&self) -> Result<IndexReader> {
        self.index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .context("open index reader")
    }

    fn build_query(
        &self,
        searcher: &tantivy::Searcher,
        tokens: &[String],
    ) -> Result<Box<dyn Query>> {
        let searchable = [
            (self.fields.title, 4.0_f32),
            (self.fields.first_prompt, 2.3_f32),
            (self.fields.conversation, 1.0_f32),
        ];
        let mut must_tokens: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        let mut expansion_budget = MAX_FUZZY_EXPANSIONS;
        for (position, token) in tokens.iter().enumerate() {
            let mut alternatives: Vec<(Occur, Box<dyn Query>)> = Vec::new();
            for (field, field_boost) in searchable {
                let term = Term::from_field_text(field, token);
                alternatives.push((
                    Occur::Should,
                    Box::new(BoostQuery::new(
                        Box::new(TermQuery::new(
                            term.clone(),
                            IndexRecordOption::WithFreqsAndPositions,
                        )),
                        field_boost,
                    )),
                ));
                let is_last = position + 1 == tokens.len();
                if is_last && expansion_budget > 0 {
                    for expanded in expand_terms(
                        searcher,
                        field,
                        token,
                        0,
                        true,
                        expansion_budget.min(MAX_EXPANSIONS_PER_CLAUSE),
                    )? {
                        alternatives.push((
                            Occur::Should,
                            Box::new(BoostQuery::new(
                                Box::new(TermQuery::new(
                                    Term::from_field_text(field, &expanded),
                                    IndexRecordOption::WithFreqsAndPositions,
                                )),
                                field_boost * 0.55,
                            )),
                        ));
                        expansion_budget -= 1;
                    }
                }
                if let Some(distance) = fuzzy_distance(token)
                    && expansion_budget > 0
                {
                    for expanded in expand_terms(
                        searcher,
                        field,
                        token,
                        distance,
                        false,
                        expansion_budget.min(MAX_EXPANSIONS_PER_CLAUSE),
                    )? {
                        alternatives.push((
                            Occur::Should,
                            Box::new(BoostQuery::new(
                                Box::new(TermQuery::new(
                                    Term::from_field_text(field, &expanded),
                                    IndexRecordOption::WithFreqsAndPositions,
                                )),
                                field_boost * 0.35,
                            )),
                        ));
                        expansion_budget -= 1;
                    }
                }
            }
            must_tokens.push((Occur::Must, Box::new(BooleanQuery::new(alternatives))));
        }

        if tokens.len() > 1 {
            for (field, boost) in searchable {
                let terms = tokens
                    .iter()
                    .map(|token| Term::from_field_text(field, token))
                    .collect();
                must_tokens.push((
                    Occur::Should,
                    Box::new(BoostQuery::new(
                        Box::new(PhraseQuery::new(terms)),
                        boost * 1.8,
                    )),
                ));
            }
        }
        Ok(Box::new(BooleanQuery::new(must_tokens)))
    }

    fn session_document(&self, key: &str, session: &Session) -> TantivyDocument {
        let mut document = doc!(
            self.fields.key => key.to_owned(),
            self.fields.provider => session.provider.as_str().to_owned(),
            self.fields.session_id => session.session_id.clone(),
            self.fields.updated_at => session.updated_at,
            self.fields.transcript_path => session.transcript_path.clone(),
            self.fields.conversation => session.conversation.clone(),
        );
        add_optional_text(&mut document, self.fields.title, session.title.as_deref());
        add_optional_text(
            &mut document,
            self.fields.title_source,
            session.title_source.as_deref(),
        );
        add_optional_text(
            &mut document,
            self.fields.first_prompt,
            session.first_prompt.as_deref(),
        );
        add_optional_text(&mut document, self.fields.cwd, session.cwd.as_deref());
        add_optional_text(
            &mut document,
            self.fields.git_branch,
            session.git_branch.as_deref(),
        );
        if let Some(started_at) = session.started_at {
            document.add_i64(self.fields.started_at, started_at);
        }
        document
    }

    fn document_session(&self, document: &TantivyDocument) -> Result<Session> {
        let text = |field| {
            document
                .get_first(field)
                .and_then(|value| value.as_str())
                .map(ToOwned::to_owned)
        };
        let required =
            |field, name| text(field).ok_or_else(|| anyhow!("indexed document lacks {name}"));
        let provider = match required(self.fields.provider, "provider")?.as_str() {
            "claude" => Provider::Claude,
            "codex" => Provider::Codex,
            value => bail!("unknown indexed provider {value}"),
        };
        Ok(Session {
            provider,
            session_id: required(self.fields.session_id, "session_id")?,
            title: text(self.fields.title),
            title_source: text(self.fields.title_source),
            first_prompt: text(self.fields.first_prompt),
            cwd: text(self.fields.cwd),
            git_branch: text(self.fields.git_branch),
            started_at: document
                .get_first(self.fields.started_at)
                .and_then(|value| value.as_i64()),
            updated_at: document
                .get_first(self.fields.updated_at)
                .and_then(|value| value.as_i64())
                .ok_or_else(|| anyhow!("indexed document lacks updated_at"))?,
            transcript_path: required(self.fields.transcript_path, "transcript_path")?,
            conversation: required(self.fields.conversation, "conversation")?,
        })
    }
}

fn schema() -> Schema {
    let mut builder = Schema::builder();
    let search_text = TextOptions::default().set_stored().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(TOKENIZER_NAME)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    builder.add_text_field("key", STRING | STORED);
    builder.add_text_field("provider", STRING | STORED);
    builder.add_text_field("session_id", STRING | STORED);
    builder.add_text_field("title", search_text.clone());
    builder.add_text_field("title_source", STORED);
    builder.add_text_field("first_prompt", search_text.clone());
    builder.add_text_field("cwd", STORED);
    builder.add_text_field("git_branch", STORED);
    builder.add_i64_field("started_at", STORED);
    builder.add_i64_field("updated_at", STORED);
    builder.add_text_field("transcript_path", STORED);
    builder.add_text_field("conversation", search_text);
    builder.build()
}

fn open_index(paths: &AppPaths) -> Result<(Index, bool)> {
    let index_dir = paths.index_dir();
    AppPaths::create_private_dir(&index_dir)?;
    match Index::open_in_dir(&index_dir) {
        Ok(index) if index.schema() == schema() => {
            secure_index_tree(&index_dir)?;
            Ok((index, false))
        }
        Ok(_) | Err(_) => {
            if index_dir.exists() {
                fs::remove_dir_all(&index_dir).context("remove corrupt or incompatible index")?;
            }
            AppPaths::create_private_dir(&index_dir)?;
            let index =
                Index::create_in_dir(&index_dir, schema()).context("create search index")?;
            secure_index_tree(&index_dir)?;
            Ok((index, true))
        }
    }
}

fn secure_index_tree(path: &Path) -> Result<()> {
    for entry in
        fs::read_dir(path).with_context(|| format!("read index directory {}", path.display()))?
    {
        let entry = entry?;
        let entry_path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            set_mode(&entry_path, 0o700)?;
            secure_index_tree(&entry_path)?;
        } else if file_type.is_file() {
            set_mode(&entry_path, 0o600)?;
        }
    }
    Ok(())
}

struct RefreshLock(File);

impl RefreshLock {
    fn acquire(paths: &AppPaths) -> Result<Self> {
        let file = private_open(&paths.lock_path())?;
        file.lock_exclusive().context("lock Dredge index")?;
        Ok(Self(file))
    }
}

impl Drop for RefreshLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

fn private_open(path: &Path) -> Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("open {}", path.display()))?;
        set_mode(path, 0o600)?;
        Ok(file)
    }
    #[cfg(not(unix))]
    {
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("open {}", path.display()))
    }
}

fn load_manifest(path: &Path) -> Result<Manifest> {
    let input = File::open(path).with_context(|| format!("open manifest {}", path.display()))?;
    serde_json::from_reader(input).context("decode index manifest")
}

fn write_manifest(path: &Path, manifest: &Manifest) -> Result<()> {
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let file = private_open(&temp)?;
    file.set_len(0).context("truncate temporary manifest")?;
    let mut output = BufWriter::new(file);
    serde_json::to_writer(&mut output, manifest).context("encode index manifest")?;
    output.flush().context("flush index manifest")?;
    output.get_ref().sync_all().context("sync index manifest")?;
    set_mode(&temp, 0o600)?;
    fs::rename(&temp, path).context("atomically install index manifest")
}

fn file_key(file: &TranscriptFile) -> String {
    format!("{}:{}", file.provider.as_str(), file.path.display())
}

fn stats_from_manifest(
    manifest: &Manifest,
    paths: &AppPaths,
    changed: usize,
    removed: usize,
    failed: usize,
) -> IndexStats {
    IndexStats {
        schema_version: SCHEMA_VERSION,
        sessions: manifest
            .files
            .values()
            .filter(|entry| matches!(entry.status, ParseStatus::Indexed))
            .count(),
        transcript_bytes: manifest.files.values().map(|entry| entry.size).sum(),
        changed,
        removed,
        failed,
        path: paths.index_dir().to_string_lossy().into_owned(),
    }
}

fn add_optional_text(document: &mut TantivyDocument, field: Field, value: Option<&str>) {
    if let Some(value) = value {
        document.add_text(field, value);
    }
}

fn fuzzy_distance(token: &str) -> Option<u8> {
    match token.chars().count() {
        0..=3 => None,
        4..=7 => Some(1),
        _ => Some(2),
    }
}

struct DfaWrapper(DFA);

impl Automaton for DfaWrapper {
    type State = u32;

    fn start(&self) -> Self::State {
        self.0.initial_state()
    }

    fn is_match(&self, state: &Self::State) -> bool {
        matches!(self.0.distance(*state), Distance::Exact(_))
    }

    fn can_match(&self, state: &Self::State) -> bool {
        *state != levenshtein_automata::SINK_STATE
    }

    fn accept(&self, state: &Self::State, byte: u8) -> Self::State {
        self.0.transition(*state, byte)
    }
}

fn expand_terms(
    searcher: &tantivy::Searcher,
    field: Field,
    token: &str,
    distance: u8,
    prefix: bool,
    limit: usize,
) -> Result<Vec<String>> {
    let mut expansions = std::collections::BTreeSet::new();
    for segment in searcher.segment_readers() {
        let inverted = segment
            .inverted_index(field)
            .context("open term dictionary")?;
        let builder = LevenshteinAutomatonBuilder::new(distance, true);
        let dfa = if prefix {
            builder.build_prefix_dfa(token)
        } else {
            builder.build_dfa(token)
        };
        let mut stream = inverted
            .terms()
            .search(DfaWrapper(dfa))
            .into_stream()
            .context("search term dictionary")?;
        while stream.advance() && expansions.len() < limit {
            if let Ok(expansion) = std::str::from_utf8(stream.key())
                && expansion != token
            {
                expansions.insert(expansion.to_owned());
            }
        }
        if expansions.len() >= limit {
            break;
        }
    }
    Ok(expansions.into_iter().take(limit).collect())
}

fn filtered(session: &Session, provider: Option<ProviderFilter>, within: Option<&Path>) -> bool {
    if provider.is_some_and(|filter| !filter.matches(session.provider)) {
        return true;
    }
    if let Some(within) = within
        && let Some(cwd) = &session.cwd
        && !normalize_path(Path::new(cwd)).starts_with(within)
    {
        return true;
    }
    false
}

fn truncate<T>(items: &mut Vec<T>, limit: usize) {
    if limit > 0 {
        items.truncate(limit);
    }
}

fn best_snippet(session: &Session, tokens: &[String]) -> (&'static str, String) {
    let candidates = [
        ("title", session.title.as_deref()),
        ("first_prompt", session.first_prompt.as_deref()),
        ("conversation", Some(session.conversation.as_str())),
    ];
    for (source, text) in candidates {
        if let Some(text) = text
            && matching_word_index(text, tokens).is_some()
        {
            return (source, snippet(text, tokens));
        }
    }
    ("conversation", snippet(&session.conversation, tokens))
}

fn matching_word_index(text: &str, tokens: &[String]) -> Option<usize> {
    text.split_whitespace().position(|word| {
        let word = word
            .trim_matches(|character: char| !character.is_alphanumeric())
            .to_lowercase();
        tokens
            .iter()
            .enumerate()
            .any(|(index, token)| token_matches(&word, token, index + 1 == tokens.len()))
    })
}

fn token_matches(word: &str, token: &str, prefix: bool) -> bool {
    word == token
        || (prefix && word.starts_with(token))
        || fuzzy_distance(token)
            .is_some_and(|distance| strsim::levenshtein(word, token) <= usize::from(distance))
}

fn snippet(text: &str, tokens: &[String]) -> String {
    let clean = clean_text(text);
    let words: Vec<&str> = clean.split_whitespace().collect();
    if words.is_empty() {
        return String::new();
    }
    let center = matching_word_index(&clean, tokens).unwrap_or(0);
    let start = center.saturating_sub(10);
    let end = (center + 15).min(words.len());
    let mut value = words[start..end].join(" ");
    if start > 0 {
        value.insert(0, '…');
    }
    if end < words.len() {
        value.push('…');
    }
    value
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn fuzzy_thresholds_match_contract() {
        assert_eq!(fuzzy_distance("cat"), None);
        assert_eq!(fuzzy_distance("rust"), Some(1));
        assert_eq!(fuzzy_distance("transcript"), Some(2));
    }

    #[test]
    fn within_keeps_missing_historical_cwd() {
        let session = Session {
            provider: Provider::Claude,
            session_id: "x".into(),
            title: None,
            title_source: None,
            first_prompt: Some("hello".into()),
            cwd: None,
            git_branch: None,
            started_at: None,
            updated_at: 1,
            transcript_path: "/x".into(),
            conversation: "hello".into(),
        };
        assert!(!filtered(&session, None, Some(Path::new("/elsewhere"))));
    }

    struct TestIndex {
        temp: TempDir,
        paths: AppPaths,
    }

    impl TestIndex {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let paths = AppPaths {
                claude_root: temp.path().join("claude"),
                codex_root: temp.path().join("codex"),
                data_root: temp.path().join("data"),
            };
            fs::create_dir_all(paths.claude_root.join("projects/test")).unwrap();
            Self { temp, paths }
        }

        fn claude(&self, id: &str, title: Option<&str>, user: &str, assistant: &str, time: i64) {
            let mut lines = Vec::new();
            if let Some(title) = title {
                lines.push(json!({"type": "custom-title", "customTitle": title}).to_string());
            }
            lines.push(
                json!({
                    "type": "user",
                    "timestamp": time,
                    "cwd": self.temp.path().join("workspace"),
                    "message": {"role": "user", "content": user}
                })
                .to_string(),
            );
            lines.push(
                json!({
                    "type": "assistant",
                    "timestamp": time,
                    "message": {"role": "assistant", "content": assistant}
                })
                .to_string(),
            );
            fs::write(
                self.paths
                    .claude_root
                    .join("projects/test")
                    .join(format!("{id}.jsonl")),
                format!("{}\n", lines.join("\n")),
            )
            .unwrap();
        }

        fn engine(&self) -> Engine {
            Engine::open(self.paths.clone()).unwrap()
        }
    }

    #[test]
    fn ranking_prefers_repeated_short_exact_text_to_incidental_huge_text() {
        let corpus = TestIndex::new();
        corpus.claude(
            "short",
            None,
            "begin short",
            "nebula nebula nebula",
            1_800_000_000,
        );
        corpus.claude(
            "huge",
            None,
            "begin huge",
            &format!(
                "{} nebula {}",
                "padding ".repeat(4_000),
                "padding ".repeat(4_000)
            ),
            1_800_000_000,
        );
        let engine = corpus.engine();
        engine.refresh(false).unwrap();
        let results = engine.search("nebula", 0, None, None).unwrap();
        assert_eq!(results[0].session_id, "short");
        assert_eq!(results[1].session_id, "huge");
    }

    #[test]
    fn ranking_prefers_title_exact_and_recent_comparable_matches() {
        let corpus = TestIndex::new();
        corpus.claude(
            "title",
            Some("galaxy"),
            "start title",
            "done",
            1_700_000_000,
        );
        corpus.claude(
            "body",
            Some("ordinary"),
            "start body",
            "galaxy",
            1_800_000_000,
        );
        corpus.claude(
            "old",
            Some("ordinary"),
            "start old",
            "comparable",
            1_700_000_000,
        );
        corpus.claude(
            "recent",
            Some("ordinary"),
            "start recent",
            "comparable",
            1_800_000_000,
        );
        let engine = corpus.engine();
        engine.refresh(false).unwrap();
        let title_results = engine.search("galaxy", 0, None, None).unwrap();
        assert_eq!(title_results[0].session_id, "title");
        assert_eq!(title_results[0].match_source.as_deref(), Some("title"));
        let recency_results = engine.search("comparable", 0, None, None).unwrap();
        assert_eq!(recency_results[0].session_id, "recent");
    }

    #[test]
    fn exact_beats_fuzzy_and_ties_are_stable() {
        let corpus = TestIndex::new();
        corpus.claude(
            "exact",
            Some("ordinary"),
            "start exact",
            "transcript",
            1_800_000_000,
        );
        corpus.claude(
            "fuzzy",
            Some("ordinary"),
            "start fuzzy",
            "transcrupt",
            1_800_000_000,
        );
        corpus.claude(
            "a-tie",
            Some("ordinary"),
            "start a",
            "deterministic",
            1_800_000_000,
        );
        corpus.claude(
            "b-tie",
            Some("ordinary"),
            "start b",
            "deterministic",
            1_800_000_000,
        );
        let engine = corpus.engine();
        engine.refresh(false).unwrap();
        let spelling = engine.search("transcript", 0, None, None).unwrap();
        assert_eq!(spelling.len(), 2);
        assert_eq!(spelling[0].session_id, "exact");
        assert_eq!(spelling[1].session_id, "fuzzy");
        let ties = engine.search("deterministic", 0, None, None).unwrap();
        assert_eq!(ties[0].session_id, "a-tie");
        assert_eq!(ties[1].session_id, "b-tie");
    }

    #[test]
    fn injected_instructions_do_not_create_search_matches_and_refresh_is_incremental() {
        let corpus = TestIndex::new();
        corpus.claude(
            "wrapped",
            Some("ordinary"),
            "<system-reminder>forbiddenneedle</system-reminder>",
            "response",
            1_800_000_000,
        );
        corpus.claude(
            "real",
            Some("ordinary"),
            "actual prompt",
            "response",
            1_800_000_000,
        );
        let engine = corpus.engine();
        let first = engine.refresh(false).unwrap();
        assert_eq!(first.stats.changed, 2);
        assert_eq!(first.stats.sessions, 1);
        let second = engine.refresh(false).unwrap();
        assert_eq!(second.stats.changed, 0);
        let manifest_path = corpus.paths.manifest_path();
        let mut legacy_manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        legacy_manifest
            .as_object_mut()
            .unwrap()
            .remove("content_version");
        fs::write(
            &manifest_path,
            serde_json::to_vec(&legacy_manifest).unwrap(),
        )
        .unwrap();
        let upgraded = engine.refresh(false).unwrap();
        assert_eq!(upgraded.stats.changed, 2);
        assert_eq!(upgraded.stats.sessions, 1);
        assert!(
            engine
                .search("forbiddenneedle", 0, None, None)
                .unwrap()
                .is_empty()
        );
        fs::remove_file(corpus.paths.claude_root.join("projects/test/real.jsonl")).unwrap();
        assert_eq!(
            engine.search("actual", 0, None, None).unwrap()[0].session_id,
            "real",
            "queries must use the index rather than reopening transcripts"
        );
        let removed = engine.refresh(false).unwrap();
        assert_eq!(removed.stats.removed, 1);
        assert_eq!(removed.stats.sessions, 0);
    }
}
