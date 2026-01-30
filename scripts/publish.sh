#!/usr/bin/env bash
# One-click publish workspace crates to crates.io.
# Usage:
#   CRATES_IO_TOKEN=your_token ./scripts/publish.sh
#   ./scripts/publish.sh your_token
#   ./scripts/publish.sh --version 0.2.0 [token]
#   ./scripts/publish.sh -v 0.2.0 --dry-run
#   CRATES_IO_TOKEN=xxx ./scripts/publish.sh -v 1.0.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT_DIR"

# Publish order: dependencies first (topological order)
CRATES=(
  bpm-core
  bpm-storage
  bpm-bpmn
  bpm-worker-sdk
  bpm-runtime
  bpm-adapter-memory
  bpm-server-rest
  bpm-engine
)

# Parse args: --version / -v, --dry-run, token
CRATES_IO_TOKEN="${CRATES_IO_TOKEN:-}"
DRY_RUN=false
VERSION=""
prev=""
for arg in "$@"; do
  if [[ "$prev" == "--version" || "$prev" == "-v" ]]; then
    VERSION="$arg"
    prev=""
    continue
  fi
  case "$arg" in
    --version|-v) prev="$arg" ;;
    --dry-run)    DRY_RUN=true ;;
    *)
      if [[ -z "$CRATES_IO_TOKEN" && "$arg" != "" ]]; then
        CRATES_IO_TOKEN="$arg"
      fi
      ;;
  esac
done
if [[ "$prev" == "--version" || "$prev" == "-v" ]]; then
  echo "Error: --version / -v requires a value (e.g. 0.2.0)"
  exit 1
fi

# If --version given, bump all workspace Cargo.toml to that version
if [[ -n "$VERSION" ]]; then
  if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?(\+[a-zA-Z0-9.-]+)?$ ]]; then
    echo "Error: invalid version '$VERSION' (expected semver, e.g. 0.2.0 or 1.0.0)"
    exit 1
  fi
  CURRENT="$(grep -m1 '^version = ' Cargo.toml | sed -n 's/^version = "\(.*\)"$/\1/p')"
  if [[ -z "$CURRENT" ]]; then
    echo "Error: could not read current version from root Cargo.toml"
    exit 1
  fi
  echo "Bumping version: $CURRENT -> $VERSION (all workspace crates and path deps)"
  while IFS= read -r -d '' f; do
    sed -i.bak "s/version = \"$CURRENT\"/version = \"$VERSION\"/g" "$f" && rm -f "${f}.bak"
  done < <(find . -name Cargo.toml -not -path './target/*' -print0)
  echo ""
fi

if [[ -z "$CRATES_IO_TOKEN" && "$DRY_RUN" != "true" ]]; then
  echo "Error: crates.io token required."
  echo "  Set CRATES_IO_TOKEN or pass token as argument."
  echo "  Example: CRATES_IO_TOKEN=xxx ./scripts/publish.sh"
  echo "  Or:      ./scripts/publish.sh --dry-run  (no token needed)"
  exit 1
fi

export CARGO_REGISTRY_TOKEN="${CRATES_IO_TOKEN}"

echo "Publishing ${#CRATES[@]} crates (root: $ROOT_DIR)"
[[ -n "$VERSION" ]] && echo "  Version: $VERSION"
if [[ "$DRY_RUN" == "true" ]]; then
  echo "  (dry-run: no token used, cargo publish --dry-run for each crate)"
fi
echo ""

for crate in "${CRATES[@]}"; do
  echo ">>> Publishing $crate"
  if [[ "$DRY_RUN" == "true" ]]; then
    cargo publish -p "$crate" --registry crates-io --dry-run 
  else
    cargo publish -p "$crate" --registry crates-io
  fi
  echo ""
done

echo "Done. All crates published successfully."
