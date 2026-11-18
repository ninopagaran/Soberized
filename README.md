# Soberized

An original, local anti-idle companion for **Sober on Arch Linux and Sway**.
The first version provides a CLI and an optional Sway shortcut. Background
anti-idle compatibility is **experimental until the host checks pass**.

Soberized creates one Xbox-compatible virtual controller, named
`Soberized Virtual Gamepad`. After observing a Roblox game join, every ten
minutes it moves the **left stick to 50% for 250 ms**, then returns every control
to neutral. Pulse direction alternates. **This briefly moves your avatar.** Park
on safe, flat ground; alternating directions cannot guarantee the same position
afterward. Whether the input resets an experience's idle timer needs testing.

Both the 0.1 camera pulse and 0.2 movement pulse failed idle testing. In the 0.2
focused diagnostic, the user saw no avatar movement despite successful kernel
writes and a separate SDL reader receiving the signal inside Sober's sandbox.
A later focused test did produce visible movement, but a further hidden-workspace
trial still ended with idle error 278. That run used the installed 0.2.0 binaries;
the 0.3.0 build had not been installed. Visible movement alone does not establish
idle prevention or prove that startup order caused the successful input test.

Version 0.3 prepares a **neutral controller before launching Sober**, waits for
its event node to be readable, and enables movement only after a game join. This
removes late controller creation from the launch path; whether it resolves
Sober/Roblox's input handling still needs a focused diagnostic and unattended
trial. The client log monitor stops input on an observed disconnect, including
idle error 278. Soberized does not reconnect automatically.

The repeated visible-but-unfocused diagnostic also produced no movement with a
terminal focused. Version 0.4 adds an optional **Gamescope nested Wayland launch**
to test focus handling independently of Sway's active window. This mode has not
yet passed the real-game background/idle checks. See [Gamescope mode](docs/GAMESCOPE.md).

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
- The session process reads a bounded portion of Sober's log to observe joins
  and disconnect reasons. It does not retain account IDs, addresses, or tickets.
  The isolated helper never reads the client log.

See [SECURITY.md](SECURITY.md) for the threat model and review details.

## Build and check

Requirements: Linux x86-64, Rust stable (1.85+), `libevdev`, `libseccomp`, Flatpak
1.15.6+, and systemd 253+. Gamescope is optional for the nested launch mode. The
initial target is Arch Linux/Sway.

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

For the experimental focus workaround, install Gamescope and a working host
Vulkan driver from Arch's repository. On this AMD Cezanne machine, use
`vulkan-radeon` (the Vulkan loader alone is insufficient). Close Sober, then use:

```sh
sudo pacman -S --needed gamescope vulkan-radeon vulkan-tools
vulkaninfo --summary
soberized launch --gamescope
```

This starts a 1280×720 nested display at 30 FPS. Sober uses its native Wayland
socket, with X11 access disabled for that launch and Gamescope's optional Vulkan
layer disabled. The compositor manages the client's internal focus; Soberized
does not switch your Sway workspace or focus. You can move the outer Gamescope
window to another workspace after validating background movement. Missing
Gamescope or a missing Wayland desktop is reported before starting a controller.
The dependency is installed separately; Soberized's installer does not fetch it.

On launch, the user worker first acquires a neutral-only lease. Once the helper
confirms preparation and udev grants normal access to the matching event node,
Sober launches with the controller already present. The worker renews its lease
while waiting for startup and a game join; no pulses or diagnostics are allowed
in that phase. Startup failure removes the controller. Status shows
`Waiting for a Roblox game join`. Join an experience
and stand in a safe location; the controller then arms. An observed Roblox error
stops input and remains visible in `soberized status` with the reason and pulse
count. After rejoining, run `soberized start` to begin a new session.

To test input delivery without waiting ten minutes:

```sh
soberized test-input
```

This queues **one fixed movement pulse after ten seconds** while a session is
armed. It is limited to one diagnostic per minute during a helper lifetime,
does not renew the session lease, and resets the next regular pulse to ten
minutes after the diagnostic. The helper's journal records the pulse start,
source, count, and neutral reset. First test with Sober visible, then while it is
on another workspace. Reading the counter alone does not prove Roblox accepted
the input or reset its idle timer.

Follow [the host validation procedure](docs/VALIDATION.md) before depending on
anti-idle behavior. An “Armed” status means the scheduler owns a lease, not that
Roblox has confirmed idle prevention.

When updating, stop the old session, rebuild, and reinstall both binaries and the
service files. Close Sober before testing the new launch order:

```sh
soberized stop
cargo build --release --locked --offline
sudo ./scripts/install.sh --user nino
systemctl --user daemon-reload
soberized --version
soberized launch --gamescope
```

Protocol version 3 rejects helpers without neutral preparation. An incompatible
helper error means the installed CLI/helper or the still-running old session must
be updated together. `start` attaches to an existing instance and cannot repeat
the controller-before-Sober startup sequence; use `launch` for this diagnostic.
Verify that the installed command reports `soberized 0.4.0` before testing this
release. Building in `target/release` does not update `/usr/bin/soberized` or the
root-owned helper. Status, doctor, and new runtime journal records include their
build version to make this distinction visible.
The 0.4 release uses protocol version 3 and includes the neutral preparation
stage. The installed 0.2 CLI/helper must both be replaced before trying it.

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

For a pulse history, use `journalctl -u soberized-helper.service -b --grep='Soberized pulse='`.
Each successful `phase=deflect` record should have a corresponding `phase=neutral`
record about 250 ms later, subject to scheduling latency. Disconnect timestamps
in status come from Roblox's log in UTC; journal timestamps use your local zone.

Closing Sober without `soberized stop` ends its input lease automatically. The
helper socket stays available for a later launch. If `status` reports a failed
helper or socket, inspect the journal; restarting the CLI does not repair a
service startup failure.

After correcting a startup failure, clear both failed units and restart the socket:

```sh
sudo systemctl reset-failed soberized-helper.service soberized-helper.socket
sudo systemctl restart soberized-helper.socket
```

For an older installation reporting `Failed at step FDS` and permission denied
opening `/dev/uinput`, rebuild and rerun the current installer. The helper needs
the narrow `DeviceAllow=/dev/uinput rw` cgroup allowance alongside `OpenFile=`;
its private device namespace and runtime syscall filter remain enabled. The
installer clears old failure counters after updating the service definitions.

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
