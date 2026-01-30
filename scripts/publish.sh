#!/usr/bin/env bash
# One-click publish workspace crates to crates.io.
# Usage:
#   CRATES_IO_TOKEN=your_token ./scripts/publish.sh
#   CARGO_REGISTRY_TOKEN=your_token ./scripts/publish.sh
#   ./scripts/publish.sh your_token
#   ./scripts/publish.sh --version 0.2.0 [token]
#   ./scripts/publish.sh -v 0.2.0 --dry-run

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT_DIR"

# Publish order: dependencies first (topological order)
CRATES=(
  bpm-engine-core
  bpm-engine-storage
  bpm-engine-bpmn
  bpm-engine-worker-sdk
  bpm-engine-runtime
  bpm-engine-adapter-memory
  bpm-engine-server-rest
  bpm-engine
)

# Parse args: --version / -v, --dry-run, token
# Token: CRATES_IO_TOKEN or CARGO_REGISTRY_TOKEN env, or first non-option argument
CRATES_IO_TOKEN="${CRATES_IO_TOKEN:-}"
TOKEN="${CRATES_IO_TOKEN:-${CARGO_REGISTRY_TOKEN:-}}"
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
      if [[ -z "$TOKEN" && -n "$arg" ]]; then
        TOKEN="$arg"
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

if [[ -z "$TOKEN" && "$DRY_RUN" != "true" ]]; then
  echo "Error: crates.io token required."
  echo "  Set CRATES_IO_TOKEN or CARGO_REGISTRY_TOKEN, or pass token as argument."
  echo "  Example: CRATES_IO_TOKEN=xxx ./scripts/publish.sh"
  echo "  Or:      CARGO_REGISTRY_TOKEN=xxx ./scripts/publish.sh"
  echo "  Or:      ./scripts/publish.sh --dry-run  (no token needed)"
  exit 1
fi

export CARGO_REGISTRY_TOKEN="${TOKEN}"

echo "Publishing ${#CRATES[@]} crates (root: $ROOT_DIR)"
[[ -n "$VERSION" ]] && echo "  Version: $VERSION"
if [[ "$DRY_RUN" == "true" ]]; then
  echo "  (dry-run: no token used, cargo publish --dry-run for each crate)"
  echo "  Note: dry-run may fail for crates that depend on others (e.g. bpm-storage) until those deps are on crates.io."
fi
echo ""

# After -v bump, Cargo.toml files are modified; allow publish without committing
ALLOW_DIRTY=()
[[ -n "$VERSION" ]] && ALLOW_DIRTY=(--allow-dirty)

for crate in "${CRATES[@]}"; do
  echo ">>> Publishing $crate"
  if [[ "$DRY_RUN" == "true" ]]; then
    cargo publish -p "$crate" --registry crates-io "${ALLOW_DIRTY[@]}" --dry-run
  else
    cargo publish -p "$crate" --registry crates-io "${ALLOW_DIRTY[@]}"
  fi
  echo ""
done

echo "Done. All crates published successfully."
