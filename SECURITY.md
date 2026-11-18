# Security model

Soberized is an original implementation. No Roblox Caffeine source is included.
This document describes intended boundaries, not an independent security audit.

## Assets and attackers

Protect the user's files/credentials, physical input, desktop control, and root
privileges. Relevant threats are a malicious local client, another local user,
malformed packets, compromised frontend code, and a compromised runtime helper.
There is no application network endpoint for a remote attacker to connect to.

A malicious process already running as the selected desktop UID can arm/stop the
permitted gamepad operation and cause denial of service. Socket authentication
cannot distinguish trusted applications within the same UID. Root compromise,
kernel bugs, a malicious administrator/build/dependency, and compromises inside
Sober are outside the boundary this companion can establish. System isolation
does not make an untrusted release binary safe to install as administrator.

## Privilege boundary

The root system manager opens `/dev/uinput` using `OpenFile=`. The helper is
started as a dynamic unprivileged user, with no Linux capabilities. No udev
permission changes, broad group membership, setuid executable, or persistent
Flatpak override is installed.

The closed device cgroup explicitly allows `/dev/uinput rw` so systemd can
prepare the inherited handle before entering the private device namespace.
`PrivateDevices=yes` remains enabled; this allowance does not expose a device
path to the running helper or grant permissions to the desktop account. The
runtime filter still forbids reopening/reconfiguring devices after setup.

The helper verifies named activation descriptors, the Linux uinput character
device major/minor (`10:223`), and the listener's domain/type/listening state.
It creates a fixed libevdev descriptor containing gamepad buttons and absolute
axes only. Keyboard, mouse, relative motion, force-feedback, and media key
capabilities are absent.

Before accepting any client, it disables dumpability and installs a native-ABI
default-deny libseccomp filter. The allowed syscall list lives in
`src/sandbox.rs`. It excludes all file opening, ioctls, socket creation/connect,
exec/fork/clone, ptrace, descriptor duplication/transfer, and io_uring. Blocking
ioctls also prevents destroying/recreating the existing device with different
capabilities. The filter cannot inspect pointed-to input event contents, so the
kernel-enforced fixed device capabilities remain essential.

The remaining memory-management calls are bounded by systemd's memory/task
limits and `MemoryDenyWriteExecute`. `PrivateDevices` hides other device nodes;
`PrivateNetwork` and `RestrictAddressFamilies=none` isolate networking;
`ProtectHome` hides user data; `ProtectSystem=strict` prevents filesystem writes.
The helper has no user-controlled settings, environment hooks, or plugin loader.

Cleanup first attempts neutral events and frees libevdev's bookkeeping. Its
usual destroy ioctl is denied after lockdown; closing the uniquely owned uinput
handle destroys the kernel device. SIGTERM/crashes also close this handle. This
device-lifetime property must be checked on the host during validation.

## Local protocol and timing

The pathname socket is created by systemd in root-owned `/run/soberized`, with
mode `0600` and the configured desktop owner. Both ends use `SO_PEERCRED`.
The client expects the listening endpoint's creator UID to be root; the helper
expects the root-configured desktop UID. These are kernel credentials, not a UID
supplied in a request.

Transport is `AF_UNIX/SOCK_SEQPACKET`. Requests are eight bytes and replies sixteen,
including a versioned header and reserved bytes that must be zero. Unknown
commands, truncated/oversized packets, and unexpected fields close the client.
`MSG_TRUNC` detects a valid-looking prefix with extra data. `recvmsg/sendmsg` are
not allowed; no descriptor exchange is part of the protocol.

Only Prepare, Arm, Renew, Stop, Status, and a fixed TestInput diagnostic exist.
Protocol version 3 rejects helpers without neutral preparation. Prepare acquires
the same single-owner, 30-second lease as Arm but permits no scheduled or test
pulses. Only that owner can arm the prepared controller; another Prepare cannot
replace it or postpone an armed pulse. A single connection owns the lease;
another client cannot renew or replace it. Authorized clients may stop it. The
helper permits at most eight clients, bounded acceptance work, a five-second
unowned startup window, and five-second idle non-owner connections. Systemd also
limits CPU, memory, tasks, descriptors, and activation frequency.

Pulse parameters are fixed: 600 seconds between scheduled starts, left-stick X
amplitude 16384, 250 ms duration, alternating direction, full neutral reset. No
initial pulse occurs on arm. An authorized diagnostic request queues this same
pulse after ten seconds, requires an armed owner lease, and has a one-minute
cooldown during the helper lifetime. It cannot renew that lease or provide raw
events, axes, amplitudes, durations, or arbitrary timing. A queued diagnostic
replaces the next scheduled pulse so the two do not collide. A local attacker
with the desktop UID can stop/restart the helper, resetting its diagnostic
cooldown; no cross-restart rate-limit guarantee is made.
The normal 50 ms event-loop polling can add scheduling latency; this is not a
hard real-time system. An exploited helper could write arbitrary events within
its fixed gamepad capabilities; fixed application timing is not a security
boundary against arbitrary code execution inside the helper.

Leases last 30 seconds and renew every 10 seconds. Both helper and controller use
`CLOCK_BOOTTIME`, including suspended time. Gaps above five seconds terminate the
session, and expired leases are checked before renewal. No catch-up is attempted.
The controller binds to one Flatpak instance ID/PID and stops on loss/change.
For `launch`, it first prepares the neutral controller and waits up to 30 seconds
for Sober to appear. The launcher waits for a readable gamepad event node before
starting Flatpak. This uses sysfs names, character-node metadata, and an access
check; it does not open or read physical input devices, change permissions, or
change the helper's syscall filter. Launch errors stop the session worker.

The unprivileged session controller observes joins and disconnects in Sober's
engine log. It waits for a join before arming and drops its lease on an observed
disconnect. Logs are untrusted: only fixed engine header/category patterns are
parsed, lines and per-poll reads are bounded, and no log content is executed.
Only a validated timestamp and numeric reason are retained in status. The helper
does not read these logs. Missing logs cause waiting; read failures stop the
session. Game connection state cannot be proven by this heuristic. The initial
snapshot is limited to the last 8 MiB, with subsequent reads limited to 256 KiB
per poll and individual lines limited to 8 KiB.

Each successful output and neutral reset is journaled with its pulse count,
source, and boottime, and shutdown records its cause. The user controller reports
disconnect details using systemd StatusText, with notification field injection
rejected. No account IDs, server addresses, tickets, or full game log lines are
copied to the journal or status.

## Optional nested compositor

`launch --gamescope` executes the fixed `/usr/bin/gamescope` profile as the
desktop user, outside the restricted helper. It is an additional trusted host
component and must be kept updated. Its nested desktop provides focus handling;
it is not an additional security sandbox for Sober. Only Sober is launched in it.

Gamescope uses the Wayland backend and exposes its native Wayland socket to its
Flatpak child. The child enables that socket, disables X11/fallback-X11 sockets,
clears `DISPLAY`, and selects SDL's Wayland driver. Gamescope's own environment
sets `WAYLAND_DISPLAY` to the nested socket before executing Flatpak. The
optional Gamescope Vulkan WSI layer is explicitly disabled. No host display
focus commands, keyboard/cursor grab options, real-time capabilities, DRM
backend, file-sharing grants, broad device access, or persistent overrides are
requested. Launch arguments and application ID are fixed; arbitrary command or
flag passthrough is rejected. Dependency/desktop checks precede lease creation.

The original helper's identity, syscall filter, device capabilities, protocol,
timing, and lease rules are unchanged. The virtual controller remains visible
to other applications with normal device access; a nested compositor does not
turn it into Sober-only input. Background acceptance and idle-timer reset must
still be measured independently. See [Gamescope validation](docs/GAMESCOPE.md).

## Review and release checks

- Run `scripts/check.sh` and `scripts/check-host.sh` as an unprivileged user.
- Review every FFI/unsafe block, capabilities, filter rules, protocol, and installer.
- Review locked Rust dependencies and system libevdev/libseccomp advisories.
  Installers must not fetch or compile code as root.
- Run the negative permission checks and actual controller lifetime checks in
  `docs/VALIDATION.md`; unit tests do not prove systemd isolation or Sober behavior.
- Keep installed binaries/unit files root-owned and outside user-writable paths.
- Record the versions and findings for the exact release being tested. Do not
  advertise Sober-only input, universal background support, or ban immunity.

## Primary references

- [Linux uinput and libevdev](https://docs.kernel.org/input/uinput.html)
- [systemd OpenFile](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html#OpenFile=)
- [Systemd service isolation](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html)
- [Seccomp filtering and its limits](https://docs.kernel.org/userspace-api/seccomp_filter.html)
- [Unix socket credentials and message semantics](https://man7.org/linux/man-pages/man7/unix.7.html)
- [Flatpak input permissions](https://docs.flatpak.org/en/latest/sandbox-permissions.html)
- [Sober gamepad setting](https://vinegarhq.org/Sober/Configuration/index.html)
- [SDL background joystick events](https://wiki.libsdl.org/SDL3/SDL_HINT_JOYSTICK_ALLOW_BACKGROUND_EVENTS)
- [Gamescope focus behavior](https://github.com/ValveSoftware/gamescope/issues/636#issuecomment-1264253700)
- [Gamescope native Wayland focus handling](https://github.com/ValveSoftware/gamescope/blob/master/src/wlserver.cpp)
- [Gamescope optional layer activation](https://github.com/ValveSoftware/gamescope/blob/master/layer/VkLayer_FROG_gamescope_wsi.json.in)
