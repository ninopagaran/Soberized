#!/bin/sh
set -eu
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH
[ "$(id -u)" = 0 ] || { printf '%s\n' 'Run this uninstaller with sudo.' >&2; exit 1; }
systemctl disable --now soberized-helper.socket 2>/dev/null || true
systemctl stop soberized-helper.service 2>/dev/null || true
rm -f /etc/systemd/system/soberized-helper.socket /etc/systemd/system/soberized-helper.service
rm -f /usr/lib/systemd/user/soberized-session.service /etc/modules-load.d/soberized.conf
rm -f /usr/bin/soberized /usr/lib/soberized/soberized-helper
rmdir /usr/lib/soberized /run/soberized 2>/dev/null || true
systemctl daemon-reload
printf '%s\n' 'Removed. Run systemctl --user daemon-reload as your desktop user.' 'Remove the optional Sway shortcut if you added it. Existing device permissions and Flatpak overrides were not changed.'
