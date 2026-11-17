#!/bin/sh
set -eu
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH
umask 022

fail() { printf '%s\n' "install: $*" >&2; exit 1; }
[ "$(id -u)" = 0 ] || fail 'run this installer with sudo after building as your normal user'
[ "$#" = 2 ] && [ "$1" = --user ] || fail 'usage: sudo ./scripts/install.sh --user YOUR_DESKTOP_USER'
owner=$2
case "$owner" in ''|*[!a-zA-Z0-9_.-]*|[-0-9]*) fail 'invalid desktop user name' ;; esac
owner_uid=$(id -u "$owner") || fail 'desktop user does not exist'
[ "$owner_uid" -ge 1000 ] && [ "$owner_uid" -lt 60000 ] || fail 'choose a regular desktop user (UID 1000-59999)'
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
for artifact in soberized soberized-helper; do
    binary="$project_dir/target/release/$artifact"
    [ -f "$binary" ] && [ -x "$binary" ] && [ ! -L "$binary" ] || fail "missing regular release binary: $binary; build without sudo first"
done
for command in flatpak systemctl systemd-analyze; do command -v "$command" >/dev/null || fail "missing dependency: $command"; done
systemd_version=$(systemctl --version | awk 'NR==1 {print $2}')
[ "$systemd_version" -ge 253 ] || fail 'systemd 253 or later is required for OpenFile='
flatpak_version=$(flatpak --version | awk '{print $2}')
version_ok=$(printf '%s\n' 1.15.6 "$flatpak_version" | sort -V | head -n 1)
[ "$version_ok" = 1.15.6 ] || fail 'Flatpak 1.15.6 or later is required for --device=input'

# Fail before replacing files or stopping services when the kernel device cannot
# be used. Do not consult a removed module tree for an already registered driver.
# shellcheck source=scripts/lib/uinput.sh
. "$project_dir/scripts/lib/uinput.sh"
ensure_uinput /sys/class/misc/uinput/dev /dev/uinput /lib/modules "$(uname -r)"

# Only root-owned system paths are installed. No build, download, user-group
# change, setuid binary, or device permission change is performed here.
systemctl stop soberized-helper.socket soberized-helper.service 2>/dev/null || true
install -Dm755 -o root -g root "$project_dir/target/release/soberized" /usr/bin/soberized
install -Dm755 -o root -g root "$project_dir/target/release/soberized-helper" /usr/lib/soberized/soberized-helper
install -Dm644 -o root -g root "$project_dir/packaging/soberized-session.service" /usr/lib/systemd/user/soberized-session.service
install -d -m755 -o root -g root /etc/systemd/system
sed "s/@OWNER@/$owner/g" "$project_dir/packaging/soberized-helper.socket.in" > /etc/systemd/system/soberized-helper.socket
sed "s/@OWNER_UID@/$owner_uid/g" "$project_dir/packaging/soberized-helper.service.in" > /etc/systemd/system/soberized-helper.service
chown root:root /etc/systemd/system/soberized-helper.socket /etc/systemd/system/soberized-helper.service
chmod 644 /etc/systemd/system/soberized-helper.socket /etc/systemd/system/soberized-helper.service
install -d -m755 -o root -g root /etc/modules-load.d
printf '%s\n' uinput > /etc/modules-load.d/soberized.conf
chmod 644 /etc/modules-load.d/soberized.conf
systemd-analyze verify /etc/systemd/system/soberized-helper.socket /etc/systemd/system/soberized-helper.service
[ ! -L /run/soberized ] || fail '/run/soberized must not be a symlink'
install -d -m755 -o root -g root /run/soberized
systemctl daemon-reload
systemctl enable --now soberized-helper.socket
printf '%s\n' 'Installed. As your desktop user, run:' '  systemctl --user daemon-reload' '  soberized doctor --sandbox-test' 'Then enable gamepad access in Sober Settings and follow docs/VALIDATION.md.'
