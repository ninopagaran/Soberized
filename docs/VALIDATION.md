# Host validation gates

Status: **not yet validated on the host or in a Roblox experience**.
The development agent's sandbox hides `/dev/input` and `/dev/uinput`, blocks
Unix socket binding/credential queries, and prevents managing host services. Build/unit-test success is
not evidence of background anti-idle compatibility.

## 1. Before creating a controller

Run as the desktop user:

```sh
./scripts/check.sh
./scripts/check-host.sh
```

Record `uname -r`, `sway --version`, `flatpak --version`, `flatpak info
org.vinegarhq.Sober`, `systemctl --version`, and the build version. Record whether
the current user already has `input` group membership or access to `/dev/uinput`.
Soberized must not add either permission. A pre-existing broad permission is
outside Soberized's isolation boundary; inspect its origin separately.

Inspect the installer and unit files, build the release, install, then run:

```sh
systemctl --user daemon-reload
soberized doctor --sandbox-test
systemd-analyze security soberized-helper.service
systemctl show soberized-helper.socket -p ActiveState
```

The host tests must pass, including the otherwise-ignored IPC checks. The
helper socket must be root-created, mode `0600`, owned by the selected desktop
user, with a root-owned parent directory. The helper must be absent until used.
An offline systemd security score is a review aid, not proof of confinement.

## 2. Prove the fixed gamepad reaches Sober

Enable gamepad support through `flatpak run org.vinegarhq.Sober config`. Close any
running Sober instance, then run `soberized launch` and join a controller-enabled
experience. Check `soberized status` and both service journals.

Use `/proc/bus/input/devices` or the corresponding sysfs device to identify the
controller by `Soberized Virtual Gamepad`. Verify one device only, the intended
gamepad capabilities, and absence of keyboard/mouse/relative capabilities.
Verify access using normal joystick ACL handling; **do not fix a failure with
`chmod 666`, input group membership, a root runtime, or `--device=all`**.

Wait ten minutes for the first pulse. Confirm the camera reacts and returns to
rest after roughly 250 ms plus scheduler latency. There must be no continuing
movement, held buttons, desktop typing, mouse movement, or focus change. Confirm
which behavior the right-stick pulse produces in the chosen experience.

If discovery, input delivery, or timer reset fails, record the exact failure and
stop at this gate. No automatic escalation or alternative injection is built in.

## 3. Prove background anti-idle behavior

Record the experience/place ID and its ordinary idle behavior without Soberized.
Use an experience with controller support and a standard inactivity timeout;
an experience-specific AFK mechanic is a separate compatibility question.

Perform separate runs, each at least 30 minutes, without real input into Sober:

1. Keep Sober unfocused on a visible workspace while typing/browsing elsewhere.
2. Move Sober to another Sway workspace and work normally.
3. Move Sober into the Sway scratchpad and work normally.

Record whether the instance stays connected, its pulse count, visible camera
effects, focus changes, and desktop interference. Do not mark a failed mode as
supported. No universal minimized/background claim is established by SDL's hint
alone. Test other games only after stopping Soberized.

## 4. Failure, permissions, and removal

- `soberized stop`: verify the virtual controller disappears and no further
  input occurs. A later status request must not arm a session.
- Run `systemctl --user kill soberized-session.service`: the owner disconnect
  must remove the device promptly. Repeat during a scheduled pulse.
- Close Sober and reopen it: the old session must stop and must not attach to
  the new instance automatically.
- Suspend/resume: verify neutral/device removal and no pulse replay; resume
  requires explicit start. Run `soberized stop` before locking via your lock hook.
- Open a second client: another Arm must be Busy and its Renew must not extend
  the lease. A malformed/oversized owner message must end that lease.
- Verify a different desktop UID cannot connect or control the helper.
- Inspect the running helper's `Uid`, `CapEff`, `NoNewPrivs`, and `Seccomp` in
  `/proc/PID/status` as administrator. It must be non-root, without capabilities,
  with no-new-privileges and filtering active. Inspect its mount/network
  namespaces and descriptors; it must not access physical input or home files.
- Uninstall and reload the user manager. Confirm units/binaries/socket/module
  configuration are removed, no controller remains, and original device
  permissions/Flatpak overrides are unchanged.

Keep these results with the tested release. An “Armed” status or passing syscall
test alone does not complete these gates.
