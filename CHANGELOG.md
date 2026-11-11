# Changelog

All notable changes to Dredge are documented here.

## 0.1.1 - 2026-11-11

- Publish prebuilt binaries for Linux (x86_64, aarch64; static musl) and macOS
  (x86_64, Apple silicon) with checksums and GitHub build provenance.
- Add `install.sh`, a one-line installer that needs no Rust toolchain.

## 0.1.0 - 2026-10-22

- Add incremental private Tantivy indexing for Claude Code and Codex history.
- Add defensive transcript discovery, parsing, and conversation filtering.
- Add boosted exact, prefix, and fuzzy search with relevance/recency ranking.
- Add `index`, `search`, `list`, `status`, and `completions` commands.
- Add stable API version 1 JSON envelopes and deterministic result ordering.
