# Dredge v0 design

Status: implementation contract for the first usable release.

This document is intentionally based on Dredge's requirements, observed agent
transcript formats, and pmux's existing history behavior. The implementer must
not consult or copy the source of other agent-history search tools.

## Purpose

Dredge is a small, local-first search engine for interactive AI-agent session
history. It indexes Claude Code and Codex transcripts, ranks matches by both
relevance and recency, and emits a stable machine-readable result that tools
such as pmux can consume.

The Cargo package is `dredge-cli`; the installed executable and repository are
named `dredge`.

## Non-goals for v0

- Resuming, attaching to, killing, or otherwise controlling sessions. The
  caller owns those actions.
- A TUI, GUI, MCP server, semantic/vector search, LLM summaries, or network
  service.
- Parsing every AI client. The initial provider set is Claude Code and Codex.
- Determining whether a session is currently live. A process manager such as
  pmux has more reliable process context and joins live state by
  `(provider, session_id)`.
- Indexing spawned subagents, non-interactive SDK/`exec` runs, tool payloads,
  generated instructions, or system context as user conversation.

## User-facing CLI

```text
dredge index [--force] [--json]
dredge search <QUERY> [--limit N] [--provider claude|codex]
              [--within PATH] [--refresh auto|always|never] [--json]
dredge list [--limit N] [--provider claude|codex] [--within PATH]
            [--refresh auto|always|never] [--json]
dredge status [--json]
dredge completions <SHELL>
```

Defaults:

- Human-readable output goes to stdout unless `--json` is passed.
- Diagnostics and progress go to stderr. JSON stdout is never mixed with
  diagnostics.
- `--limit` defaults to 20. `--limit 0` means no result limit.
- `--refresh auto` scans transcript metadata and incrementally updates changed
  files before the query. `never` is intended for search-as-you-type callers
  that run `dredge index` once when opening their picker.
- `--within` includes sessions whose canonical/normalized cwd is the given
  directory or a descendant. A missing historical cwd is still searchable.
- No command makes a network request.

Exit status is 0 for a successful query, including zero matches; 2 for CLI
usage errors; and 1 for indexing, I/O, or corrupt-index failures that could not
be recovered automatically.

## Stable JSON contract

JSON output is a single object, never bare rows or JSON Lines. Additive fields
are permitted within an API version. Renames, removals, or semantic changes
require a new `api_version`.

### `search --json`

```json
{
  "api_version": 1,
  "generated_at": 1792100000,
  "query": "tmux 3.8 layout",
  "index_refreshed": true,
  "results": [
    {
      "provider": "claude",
      "session_id": "4db6b0d3-4747-4aab-8eac-6f0b83463eb5",
      "title": "Pmux architecture and release readiness",
      "title_source": "ai",
      "first_prompt": "Review the release readiness work",
      "cwd": "/home/me/pmux",
      "git_branch": "main",
      "started_at": 1792000000,
      "updated_at": 1792090000,
      "transcript_path": "/home/me/.claude/projects/...jsonl",
      "score": 0.8732,
      "match_source": "conversation",
      "snippet": "...read tmux 3.8's JSON window layouts..."
    }
  ]
}
```

Contract details:

- Timestamps are UTC Unix seconds.
- `provider` is a lowercase stable identifier, initially `claude` or `codex`.
- `session_id` is the provider's resumable session identifier.
- `title`, `title_source`, `first_prompt`, `cwd`, `git_branch`, `started_at`,
  and `snippet` may be null. `updated_at` and `transcript_path` are present.
- `title_source` is `custom`, `ai`, `index`, or `prompt` when non-null.
- `score` is finite, normalized to `[0, 1]`, and meaningful only for ordering
  results from the same query and Dredge version.
- `match_source` is `title`, `first_prompt`, or `conversation`, selecting the
  best source used for the snippet.
- Results sort by descending score, then descending `updated_at`, then
  ascending `(provider, session_id)` for deterministic ties.
- Text fields must contain no terminal escape/control sequences. Whitespace is
  collapsed in titles, first prompts, and snippets.

`list --json` uses the same envelope and result records but omits `query`, uses
`score: null`, `match_source: null`, and `snippet: null`, and sorts by newest
`updated_at` first.

`index --json` and `status --json` return:

```json
{
  "api_version": 1,
  "generated_at": 1792100000,
  "index": {
    "schema_version": 1,
    "sessions": 272,
    "transcript_bytes": 513802240,
    "changed": 3,
    "removed": 1,
    "failed": 0,
    "path": "/home/me/.local/share/dredge/index"
  }
}
```

## Source discovery

Environment variables override defaults:

- Claude root: `$CLAUDE_CONFIG_DIR`, else `$HOME/.claude`.
- Codex root: `$CODEX_HOME`, else `$HOME/.codex`.
- Dredge data: `$DREDGE_DATA_DIR`, else
  `$XDG_DATA_HOME/dredge`, else `$HOME/.local/share/dredge`.

Claude transcripts are direct `*.jsonl` children of
`<claude-root>/projects/<encoded-project>/`. Nested agent directories are
excluded. Codex transcripts are `rollout-*.jsonl` under
`<codex-root>/sessions/YYYY/MM/DD/`.

Discovery and parsing must tolerate missing roots, unreadable individual files,
unknown records, malformed individual lines, partially written final lines,
and provider format additions. One bad transcript must not abort the corpus.

Files are identified by normalized absolute path plus provider. The refresh
manifest stores file size, nanosecond mtime where available, and parsing status.
Unchanged files are not reparsed. Removed files are deleted from the search
index. Writes are atomic, and concurrent `index`/auto-refresh processes are
serialized with a lock. A schema-version mismatch rebuilds safely.

The index and manifest may contain private conversation text. Create files and
directories without group/other access (directories 0700, files 0600, subject
to platform support) and never log transcript contents.

## Transcript model and filtering

Each indexed session contains provider/session metadata plus three separately
searchable text fields:

1. `title`: newest explicit/custom title, then provider-generated title, then
   Codex thread name, then the first real user prompt.
2. `first_prompt`: first real user-authored prompt.
3. `conversation`: real user and assistant natural-language messages in
   chronological order, capped only by documented safety limits.

Tool calls/results, thinking/reasoning, system messages, progress records, file
snapshots, diffs, hook output, and injected project/global instructions are not
conversation text.

### Claude Code

- Session id comes from the transcript filename (or an agreeing record value).
- Read `cwd`, `gitBranch`, timestamps, `custom-title`, `ai-title`, and legacy
  summary/title records defensively.
- Include interactive top-level sessions that received a real user prompt.
- Exclude SDK/non-interactive entrypoints and nested subagent transcripts.
- A user record can contain strings or typed content blocks. Keep ordinary
  user text, but discard known harness/context wrappers such as
  `system-reminder`, command metadata/caveats, local command stdout, memory,
  and full injected CLAUDE.md/AGENTS.md instruction blocks.

### Codex

- Session id, cwd, and git branch primarily come from `session_meta`.
- Include interactive top-level user threads. Exclude `exec` sources and
  spawned subthreads.
- User/assistant conversation is found in message response items. Ignore
  developer/system instructions, environment context, tool traffic, reasoning,
  and user content that consists only of injected AGENTS.md/context wrappers.
- The newest matching `thread_name` in
  `<codex-root>/session_index.jsonl` supplies the index title.

Parser tests must be written from small, original synthetic fixtures. Do not
copy fixtures or parser code from another history-search project.

## Search and ranking

Use an on-disk inverted full-text index (Tantivy is the intended v0 engine),
not a scan of stored transcript bodies per query.

Tokenization is Unicode-aware and case-insensitive. Keep enough token position
or offset information to produce a short highlighted-context snippet. Exact
term matches and phrase/order proximity matter. Title and first-prompt fields
receive explicit boosts over conversation text.

Typo tolerance applies to ordinary word tokens. The initial target is edit
distance 1 for tokens of length 4-7 and distance 2 for length 8+, with exact
matches always ranked above fuzzy-only matches. The last token should support
prefix/typeahead matching. Very short tokens do not receive fuzzy expansion.
Cap fuzzy expansion so adversarial queries cannot explode in cost.

Ranking has two independently testable parts:

1. `relevance` in `[0, 1]`, derived from the text engine's score for the
   current result set, including field boosts and an exact-match advantage.
2. `recency` in `[0, 1]` using exponential decay:
   `exp(-ln(2) * age_days / 30)`, so recency halves every 30 days.

The final score is `0.82 * relevance + 0.18 * recency`. A match is never
returned from recency alone. This gives relevance authority while using age to
separate similarly relevant sessions. Constants live in one documented module
so later evaluation can tune them without changing the JSON contract.

Minimum ranking tests:

- A short session with repeated exact query terms outranks a huge session with
  one incidental occurrence.
- A title match outranks the same match only in conversation.
- Exact spelling outranks a fuzzy correction; the correction still matches.
- Given comparable relevance, a recent session outranks an old one.
- Injected instructions containing the query do not create a match.
- Stable tie ordering is deterministic.

## Performance targets

Measured on a warm index of roughly 500 MiB / 300 transcripts:

- `search --refresh never --json`: p95 under 50 ms.
- no-change incremental refresh: under 150 ms.
- cold index: linear in input size, bounded memory, and progress only on stderr.

Correctness and stable output take precedence, but tests/benchmarks should make
it difficult to accidentally replace indexed search with corpus scanning.

## pmux integration contract

pmux treats Dredge as optional:

1. Detect `dredge` on `PATH` (or a future configured backend command).
2. Refresh once when the combined attach/history picker opens.
3. Query with `dredge search --refresh never --json --limit <N> <query>`.
4. Decode only API version 1 and ignore additive unknown fields.
5. Join `(provider, session_id)` to pmux's live-process map. A live result uses
   pmux's existing attach/switch action; a finished result uses its existing
   provider resume action.
6. If Dredge is absent, times out, fails, emits invalid JSON, or reports an
   unsupported API version, keep the current picker behavior and show a concise
   non-fatal diagnostic.

Dredge contains no pmux-specific state or actions. This boundary also permits
future search backends that implement the same conceptual result mapping.

## Release quality bar

- Linux and macOS support.
- Rust 1.88+ and edition 2024 unless a dependency forces a documented change.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
  `cargo test` pass.
- `cargo package` contains only required sources, docs, license, and lockfile.
- README documents privacy, data paths, JSON stability, and the fact that
  provider transcript formats are private and parsed defensively.
- Apache-2.0 license and no source or fixtures copied from other search tools.
