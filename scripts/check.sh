#!/usr/bin/env bash
# The full local gate chain, mirroring .github/workflows/ci.yml. Run before
# every commit. The exit code is the verdict — never grep output for errors.
#
# Toolchain-dependent gates (MSRV, RustSec audit) run when their tool is
# installed and are left to CI otherwise, like the tmux tests skip without
# tmux.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt -- --check
cargo check --locked --no-default-features --lib --bin text-graph
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets

if cargo +1.95.0 --version >/dev/null 2>&1; then
    cargo +1.95.0 check --locked --all-targets
else
    echo "note: rust 1.95.0 toolchain not installed — MSRV check left to CI" >&2
fi

if cargo audit --version >/dev/null 2>&1; then
    # keep these in step with the `ignore:` list in .github/workflows/ci.yml,
    # or "mirrors CI" stops being true
    cargo audit --ignore RUSTSEC-2025-0141 --ignore RUSTSEC-2024-0320
else
    echo "note: cargo-audit not installed — advisory check left to CI" >&2
fi

echo "all gates passed"
