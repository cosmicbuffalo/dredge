# Dredge

Dredge is a small, local-first full-text search engine for interactive AI-agent
session history. It incrementally indexes Claude Code and Codex transcripts and
returns sessions ranked by text relevance and recency. It never contacts a
network service and it does not resume or control sessions.

Dredge uses [Tantivy](https://github.com/quickwit-oss/tantivy), an external
open-source Rust full-text search library, as an embedded dependency for its
on-disk index and text-relevance scoring. Tantivy is compiled into Dredge;
there is no separate search server or service to install.

This is the first usable release. Provider transcript formats are private,
evolving implementation details; Dredge parses them defensively, skips unknown
records, and treats a malformed line or one unreadable transcript as a local
indexing failure rather than aborting the corpus.

## Install

Dredge requires Rust 1.88 or newer.

```sh
cargo install --git https://github.com/cosmicbuffalo/dredge --tag v0.1.0 --locked
```

From a checkout, `cargo install --path . --locked` installs the working tree.

The Cargo package is `dredge-cli`; the installed executable is `dredge`.

## Use

```sh
dredge index
dredge search "tmux layout"
dredge search --provider codex --within "$PWD" --json "release"
dredge list --limit 50
dredge status --json
dredge completions zsh > ~/.zfunc/_dredge
```

`search` and `list` refresh changed transcripts by default. For interactive
pickers, run `dredge index` once and use `--refresh never` for each typeahead
query. `--refresh always` forces a complete reparse. `--limit` defaults to 20;
zero means unlimited. A missing historical working directory remains eligible
for `--within`, because absence is not evidence that it was outside the path.

Human-readable output goes to stdout. With `--json`, stdout contains exactly
one JSON object and diagnostics remain on stderr. Successful empty queries have
exit status zero, CLI usage errors have status 2, and unrecoverable index or I/O
errors have status 1.

## Discovery and data paths

Claude transcripts are discovered as direct `*.jsonl` children beneath
`$CLAUDE_CONFIG_DIR/projects`, defaulting to `~/.claude/projects`. Nested agent
directories are not indexed. Codex transcripts are discovered as
`rollout-*.jsonl` under `$CODEX_HOME/sessions/YYYY/MM/DD`, defaulting to
`~/.codex/sessions`.

The index is stored beneath:

1. `$DREDGE_DATA_DIR`, when set;
2. `$XDG_DATA_HOME/dredge`, when set; or
3. `~/.local/share/dredge`.

Dredge stores its Tantivy-backed inverted index in `index/` and an atomic
incremental refresh manifest in `manifest.json`. A file's normalized path,
provider, size, and nanosecond mtime identify unchanged input. A schema mismatch
or corrupt index is rebuilt safely. Concurrent refreshes are serialized with
`index.lock`.

## Privacy and safety limits

The index and manifest are private local data and may contain conversation
text. Dredge creates its data directories with mode 0700 and files with mode
0600 on Unix platforms. It never logs transcript contents and makes no network
requests. Back up, copy, and delete this directory with the same care as the
original transcripts.

Only real user and assistant natural-language messages are indexed. Tool
traffic, reasoning, system/developer messages, generated instructions, common
context wrappers, diffs, snapshots, and subagent/exec sessions are excluded.
Defensive limits cap an individual JSONL record at 8 MiB, retained conversation
text at 16 MiB per session, display titles and first prompts after 240 Unicode
scalar characters (followed by an ellipsis), and a query at 32 tokens of at
most 64 bytes each.
Ordinary query words use edit-distance fuzzy matching (one edit at lengths 4–7,
two at length 8+) and the final word also supports prefix matching.

## Ranking

Tantivy's indexed text score includes term frequency, document-length
normalization, phrase proximity, title boosts, and first-prompt boosts. Exact
clauses receive more weight than fuzzy or prefix clauses. Relevance is
normalized within the current result set. Recency uses a 30-day exponential
half-life, and the final score is:

```text
0.82 * relevance + 0.18 * recency
```

Recency can reorder comparable matches but can never create a match.

## JSON API

Every JSON command emits one object with `"api_version": 1`. Search results
contain provider, resumable session ID, title metadata, first prompt, working
directory, Git branch, timestamps, transcript path, score, match source, and a
short context snippet. `list` uses the same record shape with null score,
match-source, and snippet fields. `index` and `status` report schema version,
session and byte counts, refresh counters, and the index path.

Fields may be added within an API version. Renaming, removing, or changing the
meaning of an existing field requires a new API version. Timestamps are Unix
seconds and result text is whitespace-normalized with terminal escape/control
sequences removed.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo package
```

The implementation and its synthetic fixtures are original and do not copy
source or fixtures from other agent-history search projects.

## License

Apache-2.0. See [LICENSE](LICENSE).
