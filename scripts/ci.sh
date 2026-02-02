#!/usr/bin/env bash
# Run the same checks as GitHub Actions CI locally (before pushing).
# Usage: ./scripts/ci.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT_DIR"

echo "=== Check formatting ==="
cargo fmt -- --check

echo "=== Clippy ==="
cargo clippy --workspace --all-targets -- -D warnings

echo "=== Run tests ==="
cargo test --workspace

echo "=== Run invariant tests ==="
cargo test --test invariant_token --test invariant_join --test invariant_external_task

echo "=== CI checks passed ==="
