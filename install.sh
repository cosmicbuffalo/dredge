#!/usr/bin/env sh
# Dredge installer: downloads a prebuilt `dredge` binary from GitHub Releases,
# so no Rust toolchain is needed. Everything runs from main(), called on the
# last line, so a download truncated mid-way by `curl | sh` defines functions
# but runs nothing.
#
#   curl -fsSL https://raw.githubusercontent.com/cosmicbuffalo/dredge/main/install.sh | sh
#
# Environment:
#   DREDGE_VERSION              release to install (default: latest)
#   DREDGE_BIN_DIR              binary directory (default: ~/.local/bin)
#   DREDGE_VERIFY_ATTESTATION   auto (default): verify GitHub build provenance
#                               when an authenticated `gh` is available;
#                               1: require it; 0: skip it
set -eu

REPO="${DREDGE_REPO:-cosmicbuffalo/dredge}"
VERSION="${DREDGE_VERSION:-latest}"
BIN_DIR="${DREDGE_BIN_DIR:-$HOME/.local/bin}"

detect_target() {
  os=$(uname -s | tr '[:upper:]' '[:lower:]')
  arch=$(uname -m)

  case "$arch" in
    x86_64|amd64) arch="x86_64" ;;
    aarch64|arm64) arch="aarch64" ;;
    *)
      echo "Unsupported architecture: $arch" >&2
      echo "  Build from source instead: cargo install --git https://github.com/$REPO --locked" >&2
      exit 1
      ;;
  esac

  case "$os" in
    linux) printf '%s-unknown-linux-musl\n' "$arch" ;;
    darwin) printf '%s-apple-darwin\n' "$arch" ;;
    *)
      echo "Unsupported operating system: $os" >&2
      echo "  Build from source instead: cargo install --git https://github.com/$REPO --locked" >&2
      exit 1
      ;;
  esac
}

download() {
  url=$1
  destination=$2
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$destination"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "$destination" "$url"
  else
    echo "curl or wget is required" >&2
    exit 1
  fi
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    echo "sha256sum or shasum is required" >&2
    exit 1
  fi
}

verify_asset() {
  file=$1
  name=$2
  expected=$(awk -v name="$name" '$2 == name || $2 == "*" name {print $1; exit}' "$checksums")
  if [ -z "$expected" ]; then
    echo "Checksum entry not found for $name" >&2
    exit 1
  fi
  actual=$(sha256_file "$file")
  if [ "$actual" != "$expected" ]; then
    echo "Checksum mismatch for $name" >&2
    exit 1
  fi
}

# Check GitHub build provenance for the downloaded files. checksums.txt comes
# from the same release as the binary, so on its own it only catches corrupted
# downloads; an attestation proves the files were built by this repository's
# release workflow. Optional by default: skipped (with a note) when gh is
# missing, too old, or not logged in, and for custom DREDGE_RELEASE_BASE_URL
# mirrors. A failed verification is always fatal.
verify_attestations() {
  mode=${DREDGE_VERIFY_ATTESTATION:-auto}
  case "$mode" in
    0|no|false) return 0 ;;
    1|yes|true) required=1 ;;
    *)
      required=0
      if [ -n "${DREDGE_RELEASE_BASE_URL:-}" ]; then
        return 0
      fi
      ;;
  esac

  reason=""
  if ! command -v gh >/dev/null 2>&1; then
    reason="gh is not installed"
  elif ! gh attestation --help >/dev/null 2>&1; then
    reason="this gh has no 'attestation' command"
  elif ! gh auth status >/dev/null 2>&1; then
    reason="gh is not logged in"
  fi
  if [ -n "$reason" ]; then
    if [ "$required" -eq 1 ]; then
      echo "Cannot verify build provenance: $reason (DREDGE_VERIFY_ATTESTATION=1)" >&2
      exit 1
    fi
    echo "Skipping build provenance check: $reason."
    echo "  Verify later with: gh attestation verify \"$BIN_DIR/dredge\" --repo $REPO"
    return 0
  fi

  echo "Verifying build provenance with gh..."
  for file in "$@"; do
    if ! gh attestation verify "$file" --repo "$REPO" >/dev/null; then
      echo "Build provenance verification FAILED for $(basename "$file"); not installing." >&2
      echo "  Set DREDGE_VERIFY_ATTESTATION=0 only if you trust this source anyway." >&2
      exit 1
    fi
  done
}

main() {
  target=$(detect_target)
  asset="dredge-${target}"

  if [ -n "${DREDGE_RELEASE_BASE_URL:-}" ]; then
    download_base=${DREDGE_RELEASE_BASE_URL%/}
  elif [ "$VERSION" = "latest" ]; then
    download_base="https://github.com/${REPO}/releases/latest/download"
  else
    case "$VERSION" in
      v*) tag=$VERSION ;;
      *) tag="v${VERSION}" ;;
    esac
    download_base="https://github.com/${REPO}/releases/download/${tag}"
  fi

  tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/dredge-install.XXXXXX")
  trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM
  binary="$tmp_dir/$asset"
  checksums="$tmp_dir/checksums.txt"

  echo "Downloading dredge ${VERSION} for ${target}..."
  download "$download_base/$asset" "$binary"
  download "$download_base/checksums.txt" "$checksums"
  verify_asset "$binary" "$asset"
  verify_attestations "$binary"

  mkdir -p "$BIN_DIR"
  install -m 0755 "$binary" "$BIN_DIR/dredge"

  echo "dredge installed to $BIN_DIR/dredge"
  case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "Add $BIN_DIR to PATH to run it." ;;
  esac
  echo "Build the index once with: dredge index"
  echo "Uninstall with: rm \"$BIN_DIR/dredge\" (the index defaults to ~/.local/share/dredge)"
}

main "$@"
