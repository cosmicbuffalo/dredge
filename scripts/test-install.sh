#!/usr/bin/env sh
# Installs a dredge binary through install.sh from a local fake release and
# checks that the installed binary runs. DREDGE_INSTALL_TEST_BINARY names the
# binary to release (CI passes the real release artifact); it defaults to a
# local debug build.
set -eu

repo_dir=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/dredge-install-test.XXXXXX")
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM

case "$(uname -s):$(uname -m)" in
  Linux:x86_64|Linux:amd64) target="x86_64-unknown-linux-musl" ;;
  Linux:aarch64|Linux:arm64) target="aarch64-unknown-linux-musl" ;;
  Darwin:x86_64|Darwin:amd64) target="x86_64-apple-darwin" ;;
  Darwin:aarch64|Darwin:arm64) target="aarch64-apple-darwin" ;;
  *) echo "unsupported test platform" >&2; exit 1 ;;
esac

# Without a binary from CI, build the working tree so a stale debug binary
# from an older version never stands in for it.
if [ -n "${DREDGE_INSTALL_TEST_BINARY:-}" ]; then
  binary=$DREDGE_INSTALL_TEST_BINARY
else
  cargo build --quiet --manifest-path "$repo_dir/Cargo.toml"
  binary="$repo_dir/target/debug/dredge"
fi
expected_version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$repo_dir/Cargo.toml" | head -1)

asset="dredge-${target}"
release_dir="$tmp_dir/release"
home_dir="$tmp_dir/home"
bin_dir="$home_dir/.local/bin"
mkdir -p "$release_dir" "$home_dir"

write_checksums() {
  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$release_dir" && sha256sum "$asset" > checksums.txt)
  else
    (cd "$release_dir" && shasum -a 256 "$asset" > checksums.txt)
  fi
}
run_env() {
  env -u XDG_DATA_HOME -u DREDGE_DATA_DIR HOME="$home_dir" \
    CLAUDE_CONFIG_DIR="$home_dir/.claude" CODEX_HOME="$home_dir/.codex" \
    DREDGE_BIN_DIR="$bin_dir" DREDGE_RELEASE_BASE_URL="file://$release_dir" "$@"
}

# A checksum mismatch must abort before anything is installed.
cp "$binary" "$release_dir/$asset"
write_checksums
printf 'tampered' >> "$release_dir/$asset"
if run_env sh "$repo_dir/install.sh" >/dev/null 2>&1; then
  echo "install accepted an asset that does not match checksums.txt" >&2
  exit 1
fi
test ! -e "$bin_dir/dredge"

cp "$binary" "$release_dir/$asset"
write_checksums
run_env sh "$repo_dir/install.sh"

test -x "$bin_dir/dredge"
actual_version=$(run_env "$bin_dir/dredge" --version)
if [ "$actual_version" != "dredge $expected_version" ]; then
  echo "unexpected dredge --version: $actual_version" >&2
  exit 1
fi
# The installed binary indexes an empty history without error.
run_env "$bin_dir/dredge" index --json >/dev/null
run_env "$bin_dir/dredge" status --json | grep -q '"api_version": *1'

echo "install smoke test passed"
