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

Only Arm, Renew, Stop, and Status exist. A single connection owns the lease;
another client cannot renew or replace it. Authorized clients may stop it. The
helper permits at most eight clients, bounded acceptance work, a five-second
unarmed startup window, and five-second idle non-owner connections. Systemd also
limits CPU, memory, tasks, descriptors, and activation frequency.

Pulse parameters are fixed: 600 seconds between starts, right-stick X amplitude
8192, 250 ms duration, alternating direction, full neutral reset. No initial
pulse occurs on arm, so repeated activation cannot request immediate pulses.
The normal 50 ms event-loop polling can add scheduling latency; this is not a
hard real-time system. An exploited helper could write arbitrary events within
its fixed gamepad capabilities; fixed application timing is not a security
boundary against arbitrary code execution inside the helper.

Leases last 30 seconds and renew every 10 seconds. Both helper and controller use
`CLOCK_BOOTTIME`, including suspended time. Gaps above five seconds terminate the
session, and expired leases are checked before renewal. No catch-up is attempted.
The controller binds to one Flatpak instance ID/PID and stops on loss/change.

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
