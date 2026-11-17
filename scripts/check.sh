#!/bin/sh
set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
cd "$project_dir"
cargo fmt --all -- --check
cargo clippy --locked --offline --all-targets -- -D warnings
cargo test --locked --offline
for script in scripts/*.sh scripts/lib/*.sh; do sh -n "$script"; done
sh scripts/test-install.sh
if command -v shellcheck >/dev/null 2>&1; then
    shellcheck scripts/*.sh scripts/lib/*.sh
else
    printf '%s\n' 'NOTE: shellcheck is unavailable; shell syntax was checked with sh -n.'
fi
if command -v cargo-audit >/dev/null 2>&1; then
    cargo audit
else
    printf '%s\n' 'NOTE: cargo-audit is unavailable; dependency vulnerability audit still requires review.'
fi
printf '%s\n' 'Local checks passed. Run scripts/check-host.sh on the host before installation.'
