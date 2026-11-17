#!/bin/sh
set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
cd "$project_dir"
sh scripts/test-install.sh
cargo test --locked --offline -- --include-ignored
cargo build --release --locked --offline
target/release/soberized-helper --self-test
target/release/soberized-helper --self-test-ipc
# Validate rendered units against actual, locally built release executables.
temporary_dir=$(mktemp -d)
trap 'rm -rf -- "$temporary_dir"' EXIT HUP INT TERM
sed "s|@OWNER_UID@|$(id -u)|g; s|/usr/lib/soberized/soberized-helper|$project_dir/target/release/soberized-helper|g" \
    packaging/soberized-helper.service.in > "$temporary_dir/soberized-helper.service"
sed "s/@OWNER@/$(id -un)/g" packaging/soberized-helper.socket.in > "$temporary_dir/soberized-helper.socket"
sed "s|/usr/bin/soberized|$project_dir/target/release/soberized|g" \
    packaging/soberized-session.service > "$temporary_dir/soberized-session.service"
systemd-analyze --generators=no verify "$temporary_dir/soberized-helper.service" "$temporary_dir/soberized-helper.socket"
systemd-analyze --user --generators=no verify "$temporary_dir/soberized-session.service"
printf '%s\n' 'Host IPC, sandbox, and unit-file checks passed. No controller was created.' 'Continue with docs/VALIDATION.md for real Sober/device tests.'
