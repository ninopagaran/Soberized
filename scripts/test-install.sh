#!/bin/sh
# No root, module loading, service changes, or virtual input devices are needed.
set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
# shellcheck source=scripts/lib/uinput.sh
. "$project_dir/scripts/lib/uinput.sh"
temporary_dir=$(mktemp -d)
trap 'rm -rf -- "$temporary_dir"' EXIT HUP INT TERM

fail() { printf '%s\n' "installer regression: $*" >&2; exit 1; }
setup_case() {
    case_dir=$temporary_dir/$1
    mkdir "$case_dir"
    registration=$case_dir/uinput.dev
    device=/dev/null # Harmless existing character device for the node-type check.
    module_action=fail
}
modprobe() {
    [ "$#" = 1 ] && [ "$1" = uinput ] || fail 'unexpected module request'
    printf '%s\n' "$1" > "$case_dir/modprobe.called"
    case "$module_action" in
        register) printf '%s\n' 10:223 > "$registration" ;;
        fail) return 1 ;;
        no_registration) return 0 ;;
        *) fail 'unknown fake loader behavior' ;;
    esac
}
expect_success() {
    if ! ensure_uinput "$registration" "$device" "$case_dir/modules" test-kernel > "$case_dir/log" 2>&1; then
        cat "$case_dir/log" >&2
        fail 'preflight unexpectedly failed'
    fi
}
expect_failure() {
    if ensure_uinput "$registration" "$device" "$case_dir/modules" test-kernel > "$case_dir/log" 2>&1; then
        fail 'preflight unexpectedly succeeded'
    fi
}
expect_no_modprobe() {
    [ ! -e "$case_dir/modprobe.called" ] || fail 'an available driver must not invoke modprobe'
}

setup_case registered_after_upgrade
printf '%s\n' 10:223 > "$registration"
expect_success
expect_no_modprobe

setup_case load_driver
module_action=register
expect_success
[ -f "$case_dir/modprobe.called" ] || fail 'missing driver was not loaded'

setup_case missing_kernel_modules
expect_failure
grep -q 'Reboot into the installed kernel' "$case_dir/log" || fail 'missing reboot guidance'
grep -q 'test-kernel' "$case_dir/log" || fail 'missing running kernel in diagnostics'

setup_case loader_failure_with_modules
mkdir -p "$case_dir/modules/test-kernel"
expect_failure
grep -q 'check the matching kernel modules' "$case_dir/log" || fail 'missing module-loading guidance'

setup_case registered_but_hidden_node
printf '%s\n' 10:223 > "$registration"
device=$case_dir/missing-device
expect_failure
expect_no_modprobe
grep -q 'Run this installer on the host' "$case_dir/log" || fail 'missing host-device guidance'

setup_case unregistered_after_successful_loader
module_action=no_registration
expect_failure

setup_case regular_file_is_not_a_device
printf '%s\n' 10:223 > "$registration"
device=$case_dir/regular-file
printf '' > "$device"
expect_failure
expect_no_modprobe

printf '%s\n' 'PASS: 7 installer preflight regression cases (no host changes or input emitted).'
