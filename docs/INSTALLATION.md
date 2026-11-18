# Installation and maintenance

## Requirements

- Linux x86-64, with Arch Linux/Sway as the initial target.
- Rust stable 1.85 or later, including rustfmt and Clippy.
- `libevdev`, `libseccomp`, `libei`, `leptonica`, Flatpak 1.15.6+, and systemd 253+.
- Sober installed as `org.vinegarhq.Sober`.
- Gamescope and a working host Vulkan driver for the optional nested display.

On Arch, install the build dependencies:

```sh
sudo pacman -S --needed rustup libevdev libseccomp libei leptonica
rustup toolchain install stable --profile minimal --component rustfmt,clippy
```

Gamescope setup and GPU driver requirements are covered in
[the Gamescope guide](GAMESCOPE.md).

## Build and check

Run as your normal desktop user:

```sh
cargo fetch --locked
./scripts/check.sh
./scripts/check-host.sh
```

The host checks include a release build and the otherwise-ignored Unix socket
tests. These checks create no virtual controller and send no real input.
`check.sh` reports when optional ShellCheck or cargo-audit checks are unavailable.

For a standalone release build after dependencies have been fetched:

```sh
cargo build --release --locked --offline
```

Rust dependencies are pinned in `Cargo.lock`. Native bindings use the system's
libevdev, libseccomp, libei, and Leptonica. The image and pointer libraries are
linked only into the frontend. Review dependencies, service files, and the
installer before installation. Builds and downloads run without administrator
privileges.

## Install

```sh
sudo ./scripts/install.sh --user "$(id -un)"
systemctl --user daemon-reload
soberized --version
soberized doctor --sandbox-test
```

The installer copies two root-owned binaries, a private system socket/helper
service, a user-session service, and a module-load entry for `uinput`. Only the
socket is enabled at boot. No anti-idle session starts automatically.

The installer checks the kernel driver and `/dev/uinput` before replacing files
or stopping services. It does not build or download code as root, change device
permissions, add input-group membership, or install persistent Flatpak overrides.

Enable gamepad support in Sober Settings:

```sh
flatpak run org.vinegarhq.Sober config
```

Close Sober, then choose one launch profile:

```sh
soberized launch
# Optional nested Wayland display:
soberized launch --gamescope
# Optional camera mouse capture inside Gamescope:
soberized launch --gamescope --capture-mouse
```

See [Gamescope mode](GAMESCOPE.md) for display, mouse
capture, and host Vulkan setup.

## Session controls

| Command | Purpose |
| --- | --- |
| `soberized start` | Attach to an already prepared, running Sober instance. |
| `soberized stop` | Stop input and remove the virtual controller. |
| `soberized toggle` | Toggle the session controller. |
| `soberized status` | Show the controller state, pulse count, and next pulse. |
| `soberized test-input` | Queue one movement pulse after ten seconds. |
| `soberized doctor --sandbox-test` | Check setup and the syscall sandbox. |

`launch` prepares a neutral controller before Sober starts. Movement remains
disabled until a game join is observed. The session then sends a fixed 250 ms
movement pulse every ten minutes. An observed disconnect stops input and retains
its reason and pulse count in status. After rejoining, use `soberized start`.

The diagnostic pulse requires an armed session and is limited to once per minute
during a helper lifetime. It replaces the next scheduled pulse, putting the next
regular pulse ten minutes after the diagnostic. Reading a pulse counter does not
prove Roblox accepted the input or reset its idle timer; follow
[the validation procedure](VALIDATION.md).

Each launch grants input-device access and sets the SDL background-gamepad hint
for that Sober instance only. This access follows existing host permissions and
is not restricted to a single controller. `start` cannot retrofit device access
or repeat the controller-before-Sober launch sequence in an existing instance.

Add [the optional Sway binding](../packaging/sway.conf) to your configuration if
desired. Run `soberized stop` in your lock command before locking; screen-lock
detection is not automatic. Suspend gaps stop input and require an explicit
restart. Stop Soberized before playing another game.

## Update

Close Sober and stop the current input session before replacing installed files:

```sh
soberized stop
cargo fetch --locked
./scripts/check.sh
./scripts/check-host.sh
sudo ./scripts/install.sh --user "$(id -un)"
systemctl --user daemon-reload
soberized --version
```

Then launch your selected profile again. Building `target/release` does not
update the installed CLI or helper. Reinstall both binaries and the service
files together; incompatible protocol versions are rejected. Runtime logs and
diagnostics identify the build being used.

## Troubleshooting

Check the session and helper journals:

```sh
soberized doctor
journalctl --user -u soberized-session.service -b
journalctl -u soberized-helper.service -b
```

For pulse records:

```sh
journalctl -u soberized-helper.service -b --grep='Soberized pulse='
```

Each successful `phase=deflect` record should have a matching `phase=neutral`
record about 250 ms later, subject to scheduling latency. Disconnect timestamps
come from Roblox's log in UTC; journal timestamps use the local time zone.

Closing Sober automatically ends its input lease. The socket remains available
for a later launch. A failed helper/socket needs its startup problem corrected;
restarting the CLI alone does not repair a failed service.

After correcting the cause, clear failure counters and restart the socket:

```sh
sudo systemctl reset-failed soberized-helper.service soberized-helper.socket
sudo systemctl restart soberized-helper.socket
```

If `uinput` is unavailable and the running kernel's modules are missing, reboot
into the installed kernel and rerun the installer. An already registered driver
is reused even when a kernel upgrade removes the old module tree. See
[Arch's kernel-upgrade troubleshooting](https://wiki.archlinux.org/title/General_troubleshooting#Cannot_use_some_peripherals_after_kernel_upgrade).

For `Failed at step FDS` or permission denied opening `/dev/uinput` on an older
installation, rebuild and reinstall the current service files. The helper needs
the narrow `DeviceAllow=/dev/uinput rw` allowance alongside `OpenFile=`. Its
private device namespace and runtime syscall filter remain enabled. Do not work
around device access failures with broad permissions or a root runtime.

For Gamescope/Vulkan startup errors or camera input problems, use
[the Gamescope troubleshooting guide](GAMESCOPE.md).

## Remove

```sh
soberized stop
sudo ./scripts/uninstall.sh
systemctl --user daemon-reload
```

Remove any optional Sway binding or lock hook you added. Uninstall leaves existing
device permissions, other applications' Flatpak overrides, and the loaded
`uinput` module alone.
