# Soberized

A local anti-idle companion for **Sober on Linux**, built for Arch Linux and Sway.
It uses a virtual gamepad and an optional Gamescope display to send brief input
pulses while Roblox runs on another workspace.

Soberized prepares an Xbox-compatible controller through Linux `uinput` before
launching Sober. After a game join, it alternates a 250 ms left-stick movement
pulse every ten minutes, returning all controls to neutral afterward.
**Pulses can move your avatar; use a safe location.**

## Features

- Virtual controller input without modifying Roblox or Sober binaries.
- Optional Gamescope mode for background focus compatibility.
- Opt-in mouse capture for camera control inside Gamescope.
- Automatic input shutdown on disconnect, Sober exit, lease expiry, or suspend.
- CLI controls, diagnostics, pulse records, and optional Sway shortcuts.

## Usage

Follow the [installation guide](docs/INSTALLATION.md), then enable gamepad support
in Sober Settings and close Sober before launching:

```sh
soberized launch --gamescope
```

Add `--capture-mouse` to enable camera mouse capture when launching. It can confine
the cursor inside Gamescope; use your Sway workspace shortcut to leave the window.

Manage the input session with:

```sh
soberized status
soberized stop
```

Stop Soberized before playing another game, since other applications can also
detect its virtual controller.

Use `soberized --help` for all commands. Background compatibility depends on the
experience and display mode; see the [validation record](docs/VALIDATION.md).

## Security

Administrator access is used for installation. Runtime input is handled by a
separate unprivileged helper with fixed commands, short leases, and a restricted
syscall filter. Soberized does not require input-group membership or persistent
Flatpak overrides. See the [security model](SECURITY.md) for boundaries and limits.

## Documentation

- [Installation, updates, and troubleshooting](docs/INSTALLATION.md)
- [Gamescope setup and mouse capture](docs/GAMESCOPE.md)
- [Compatibility checks and validation results](docs/VALIDATION.md)
- [Security design](SECURITY.md)

Soberized is not affiliated with Roblox, VinegarHQ, or Microsoft.
