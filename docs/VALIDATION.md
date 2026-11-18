# Host validation gates

Status: **0.4's Gamescope mode passed visible/unfocused movement and a 30-minute
hidden-workspace trial with no logged disconnect on this machine. Versions 0.1
and 0.2 failed idle testing. Version 0.4.1 fixes a separate startup PID race.**

On 2026-11-18, the 0.2 helper wrote and reset four 250 ms movement pulses at
09:16, 09:26, 09:36, and 09:46 Asia/Manila. Roblox reported idle error 278 at
09:48:12 (`01:48:12Z`); the worker stopped after four pulses. Later focused
diagnostics produced no visible avatar movement although WASD worked with chat
and menus closed. A separate SDL reader inside Sober's sandbox could open the
virtual controller, and a running SDL reader noticed removal/addition. These
checks do not establish what Sober/Roblox consumed or why it ignored movement.

Version 0.3 removes late device creation from `launch` as a controlled test of
startup detection. It must pass the focused movement test below before another
unattended idle trial. Build, kernel-write, and sidecar SDL checks cannot pass
that gate. An earlier manual interaction also invalidated the initial idle test.

A later focused test produced visible movement. The following trial, with Sober
hidden on another workspace, still ended at 10:39:38 Asia/Manila with idle error
278 after three pulses: a diagnostic at 10:14:14 and scheduled pulses at 10:24:15
and 10:34:15. Each deflection had a neutral reset roughly 250 ms later. An audit
found `/usr/bin/soberized` and its installed helper were still the 0.2.0 build;
the repo's 0.3.0 release artifact had not been installed. This trial cannot
validate or invalidate the new launch order, and the focused success cannot be
attributed to that change. The launcher already had the SDL background hint set.

The user repeated the visible-but-unfocused movement test and again reported no
movement with the terminal focused. Version 0.4 adds `launch --gamescope` as an
experimental focus workaround. Focused movement, background movement, and idle
prevention are separate gates; the observed Gamescope result is recorded below.

The first 0.4.0 Gamescope launch failed before Sober started because the AMD host
lacked a Vulkan userspace driver: Gamescope's instance creation and an independent
minimal Vulkan probe both returned `-9`. The neutral helper stopped after zero
pulses and the device disappeared. Install the appropriate host Vulkan driver
and pass `vulkaninfo --summary` before repeating [Gamescope validation](GAMESCOPE.md).
After that installation, Vulkan enumeration and a Gamescope launch succeeded.
The user confirmed avatar movement with a terminal focused during the diagnostic
at 11:38:28 Asia/Manila. The earlier focused success on 0.2 remains separate from
this result.

## Observed Gamescope trial: 2026-11-18

The installed **Soberized 0.4.0**, Gamescope 3.17.0, and host AMD RADV/Mesa
26.2.7 completed an observation from **11:41:00 to 12:11:00 Asia/Manila**. The
same Sober Flatpak instance and helper stayed active. Gamescope was hidden on
another Sway workspace while the user worked elsewhere.

| Local time | Pulse | Source | Neutral reset after deflection |
| --- | --- | --- | --- |
| 11:38:28 | 1 | Diagnostic before the trial | 253 ms |
| 11:48:28 | 2 | Scheduled during the trial | 250 ms |
| 11:58:28 | 3 | Scheduled during the trial | 264 ms |
| 12:08:28 | 4 | Scheduled during the trial | 251 ms |

The observer sampled Sway window visibility/focus every two seconds and
instance/controller status about every ten seconds. There were **162 status
samples**. It recorded no visibility/focus or instance change during its active
sampling. Two gaps were retained in the record: 57 seconds after the observer
mistook the valid Pulsing state for failure, and approximately 127 seconds after
the tool server restarted. Neither event stopped Soberized. The user explicitly
confirmed that Roblox stayed hidden and received no manual input throughout
11:58:28–12:02:53, covering both gaps.

The final controller remained Armed with four total pulses. The continuously
running client-log monitor reported no disconnect, and a bounded engine-log
scan found no join or disconnect event during the observation. The local record
contains status/timing metadata only; it is not committed as machine-local data.

**Result:** this run passed the 30-minute hidden-workspace idle check. It remains
a single-run result using client-log observations, without independent server
confirmation. Overnight, scratchpad, other-experience, and other-machine results
are not established. The 0.4.1 startup fix was built and passed all 41 available
Rust tests separately; it was not installed during this trial.

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
After installation, check `soberized --version` against the release being tested;
a version from `target/release/soberized` alone does not identify installed code.

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
running Sober instance, then run `soberized launch`. Before joining, verify that
the virtual controller already exists and status says movement is disabled while
waiting for a join. Leave the home screen idle for more than ten minutes: the
pulse count must remain zero and `test-input` must be refused. Join a
controller-enabled experience. Check `soberized status` and both service journals.

Use `/proc/bus/input/devices` or the corresponding sysfs device to identify the
controller by `Soberized Virtual Gamepad`. Verify one device only, the intended
gamepad capabilities, and absence of keyboard/mouse/relative capabilities.
Verify access using normal joystick ACL handling; **do not fix a failure with
`chmod 666`, input group membership, a root runtime, or `--device=all`**.

Place the avatar on safe, flat ground. Run `soberized test-input` for one movement
pulse after a ten-second delay. Confirm the avatar briefly steps and returns to
rest after roughly 250 ms plus scheduler latency. There must be no continuing
movement, held buttons, desktop typing, mouse movement, or focus change. Confirm
which behavior the left-stick pulse produces in the chosen experience. Repeat
with Roblox still visible but a terminal focused beside it. Observe the pulse
without returning focus to Roblox: this distinguishes focused delivery from
background delivery without waiting for an idle timeout. Only then test another
workspace after the diagnostic cooldown has elapsed. Returning focus can itself
change idle behavior and must not be counted as proof of background input.
Check the journal for matching `phase=deflect` and `phase=neutral` records. Then
verify the normal ten-minute cadence without requesting further diagnostics.

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
- Return to the home screen before joining: status must wait for a game join and
  there must be no movement pulses. An observed idle/network disconnect must
  stop input and preserve its reason and pulse count in status while Sober stays
  open. Rejoining and starting again must use a fresh lease.
- Suspend/resume: verify neutral/device removal and no pulse replay; resume
  requires explicit start. Run `soberized stop` before locking via your lock hook.
- Open a second client: another Arm must be Busy and its Renew must not extend
  the lease. Repeat while the first owner is Prepared: no diagnostic may be
  queued until that owner arms. A malformed/oversized owner message must end
  that lease.
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
