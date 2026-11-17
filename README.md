# Soberized

An original, local anti-idle companion for **Sober on Arch Linux and Sway**.
The first version provides a CLI and an optional Sway shortcut. Background
anti-idle compatibility is **experimental until the host checks pass**.

Soberized creates one Xbox-compatible virtual controller, named
`Soberized Virtual Gamepad`. Every ten minutes, it moves the right stick to
25% for 250 ms and returns every control to neutral. Pulse direction alternates.
This can move the in-game camera; it does not intentionally move the avatar.
Whether that input resets an experience's idle timer needs testing.

## Security design

```text
Your CLI / Sway shortcut
        │ starts a user-session controller
        │ fixed commands over a private local socket
        ▼
Isolated helper (separate unprivileged identity)
        │ one fixed gamepad, fixed timing, short session lease
        ▼
Linux uinput → virtual gamepad → Sober
```

- Administrator access is used for installation only. Your account does not
  gain `input` group membership or new access to `/dev/uinput`.
- The system manager passes an open `uinput` handle to the helper. The helper
  creates its fixed controller **before reading client packets**, then installs
  a default-deny syscall filter. It cannot subsequently open files/devices,
  reconfigure the controller, create sockets, execute programs, or pass handles.
- Systemd isolates the helper's devices, home directories, network, filesystem
  writes, capabilities, and resources. Installation must remain root-owned.
- The control socket is mode `0600`, belongs to the selected desktop user,
  and sits in a root-owned directory. The helper checks the kernel-reported
  client UID; the client checks that the listening endpoint was created by root.
- There is no network listener, updater, telemetry, plugin system, raw-input
  command, or script execution API. Soberized does not read Roblox credentials
  or modify Roblox/Sober binaries or configuration files.
- Input stops on owner disconnect, a 30-second lease expiry, Sober exit/instance
  change, or a scheduling/suspend gap longer than five seconds. Resume requires
  an explicit start. No missed pulses are replayed.

See [SECURITY.md](SECURITY.md) for the threat model and review details.

## Build and check

Requirements: Linux x86-64, Rust stable (1.85+), `libevdev`, `libseccomp`, Flatpak
1.15.6+, and systemd 253+. The initial target is Arch Linux/Sway.

```sh
sudo pacman -S --needed rustup libevdev libseccomp
# If stable is not already installed:
rustup toolchain install stable --profile minimal --component rustfmt,clippy

./scripts/check.sh
cargo build --release --locked --offline
```

There is one Rust dependency, pinned in `Cargo.lock`: `libc`. Native bindings
link against the system's libevdev and libseccomp. On a fresh machine, first use
`cargo fetch --locked` as your normal user, then build offline. Review dependency
advisories before installing; `./scripts/check.sh` runs `cargo audit` if available
and explicitly reports when it is missing. Keep Arch system libraries updated.

IPC tests are intentionally host-only because restricted build environments
may block Unix sockets. **Before installation, run all tests on your host:**

```sh
./scripts/check-host.sh
```

These checks create no virtual controller and send no real input.

## Install on this machine

Review the release binaries, service templates, and installer before the single
administrator step. The installer performs no build or download as root.

```sh
sudo ./scripts/install.sh --user nino
systemctl --user daemon-reload
soberized doctor --sandbox-test
```

The installer installs two binaries, a private system socket/helper service, a
user-session service, and a module-load entry for `uinput`. Only the socket is
enabled at boot; it does not create a controller until a client connects. No
anti-idle session starts automatically. File contents are in
[packaging](packaging) and [scripts/install.sh](scripts/install.sh).

The installer checks that the kernel driver and `/dev/uinput` are available
before replacing installed files or stopping services. It reuses an already
registered driver, including after a kernel upgrade removes the old modules.
If the driver is unavailable and the running kernel's modules are missing,
reboot into the installed kernel and rerun the installer. See
[Arch's kernel-upgrade troubleshooting](https://wiki.archlinux.org/title/General_troubleshooting#Cannot_use_some_peripherals_after_kernel_upgrade).

Open Sober Settings and enable gamepad support:

```sh
flatpak run org.vinegarhq.Sober config
```

Close the currently running Sober instance once, then use:

```sh
soberized launch
soberized status
soberized stop
```

`launch` grants `--device=input` and sets `SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS=1`
for that Sober launch only. It does not write persistent Flatpak overrides.
`--device=input` exposes input devices allowed by existing host permissions;
it is narrower than `--device=all`, but is **not** a per-controller permission.
With Sober already launched with appropriate access, use `soberized start`.
Starting cannot retrofit device permissions into an existing Flatpak sandbox.

Follow [the host validation procedure](docs/VALIDATION.md) before depending on
anti-idle behavior. An “Armed” status means the scheduler owns a lease, not that
Roblox has confirmed idle prevention.

## Sway shortcut and stopping

Add the snippet from [packaging/sway.conf](packaging/sway.conf) to your existing
Sway configuration. The default suggested binding is `$mod+Shift+F12`.

Run `soberized stop` in your existing lock command before locking. There is no
automatic screen-lock detection in v1. Suspend gaps stop the session automatically.

**Stop Soberized before playing another game.** Its virtual controller is visible
to other applications with device access. The supported v1 goal is coding or
browsing while Roblox stays on another workspace or in Sway's scratchpad.

For errors:

```sh
journalctl --user -u soberized-session.service -b
journalctl -u soberized-helper.service -b
soberized doctor
```

If repeated startup failures hit systemd's rate limit, fix the reported cause,
then run `sudo systemctl reset-failed soberized-helper.service`.

## Remove

```sh
soberized stop
sudo ./scripts/uninstall.sh
systemctl --user daemon-reload
```

Remove the optional Sway shortcut/lock hook if added. Uninstall leaves existing
device permissions, other software's Flatpak overrides, and the loaded `uinput`
module alone.

Soberized is not affiliated with Roblox, VinegarHQ, or Microsoft. Limiting host
privileges does not guarantee account safety, immunity from exploits, or
compatibility with an experience's rules or custom AFK system.
