#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd -- "$project_dir"

# Build before launching so source changes are picked up as well.
cargo build --locked --release -p p2game --bin p2game -j 2
exec "${CARGO_TARGET_DIR:-$project_dir/target}/release/p2game" \
    --windowed "$@"
