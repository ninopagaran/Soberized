# Gamescope focus compatibility test

The native Sway launch produced visible movement with Roblox focused, but
repeated tests produced no movement with a terminal focused beside it. A hidden
workspace trial ended with idle disconnect 278 despite successful pulse/reset
records. This points to focus handling, without identifying whether Sober or
Roblox filters the events.

Version 0.4 adds `soberized launch --gamescope`. Gamescope runs a nested desktop
and manages focus within it. Its maintainer describes keeping clients focused
when the outer window is unfocused as intentional behavior; its native Wayland
implementation activates the selected client's surface and keyboard focus.
This is the basis for the experiment, not proof of Roblox compatibility.

## Install and launch

Run the standard repository checks and build as your normal user. Gamescope is
an optional package from Arch's official Extra repository. Soberized does not
install packages, fetch executable files, change device permissions, or change
Sober configuration.

Gamescope also needs a working **host Vulkan driver**. Sober's Flatpak graphics
runtime does not supply one to host Gamescope. This machine has an AMD Cezanne
GPU using the `amdgpu` kernel driver; its matching Vulkan userspace package is
`vulkan-radeon`. The `mesa` and `vulkan-icd-loader` packages alone do not provide
that driver. Use the GPU-appropriate driver on other machines.

Close the current Sober window completely, then run:

```sh
soberized stop
sudo pacman -S --needed gamescope vulkan-radeon vulkan-tools
vulkaninfo --summary
sudo ./scripts/install.sh --user nino
systemctl --user daemon-reload
soberized --version
soberized doctor --sandbox-test
soberized launch --gamescope
```

The installed version for the latest build must be `soberized 0.4.2`. Both root-owned binaries must
be updated; a successful build alone does not update installed code. Enable
gamepad support in Sober Settings before launching.
The Vulkan summary must succeed and list the intended GPU before proceeding.

The virtual controller is prepared first and stays neutral until a game join.
Gamescope creates a 1280×720 display with a 30 FPS limit both while focused and
unfocused. Sober uses Gamescope's native Wayland socket, with X11/fallback-X11
access disabled, `DISPLAY` unset, and SDL's Wayland backend selected for that
launch. The optional Gamescope Vulkan WSI layer is disabled. The standard profile
requests no cursor/keyboard grab or real-time privilege. The explicit mouse
capture profile below adds only Gamescope's cursor capture.

Sway sees the outer Gamescope window. Move that window using your normal Sway
bindings; a rule matching Sober's app ID might not match it. `soberized stop`
removes the controller while leaving the client window open. Closing Sober must
also stop its owner lease; validate compositor cleanup on this host.

## Validate the actual behavior

1. Join the same controller-enabled experience and stand on safe, flat ground.
   Queue `soberized test-input`, then focus Roblox during the ten-second delay.
   Verify one brief step followed by rest.
2. After the diagnostic cooldown, repeat with the outer Gamescope window visible
   beside a focused terminal. Watch without clicking back into Roblox. Verify
   that the pulse still moves the avatar and does not affect desktop input.
3. Queue another diagnostic after the cooldown, then move Gamescope to another
   workspace. Check the result, recording that returning focus can itself reset
   idle state. No position change while hidden is a failed delivery gate.
4. Only after background delivery works, start a fresh unattended trial lasting
   at least 30 minutes with Gamescope on another workspace. Work normally, with
   no real input into Roblox. Record pulse count and any client disconnect.
5. Check controller removal on stop, Sober exit, owner loss, suspend, and startup
   failure. Confirm neither focus nor keyboard/mouse input is taken from the
   host desktop. Stop Soberized before playing another game.

For display verification, inspect the new Flatpak instance's `/.flatpak-info`
and environment using `flatpak enter INSTANCE_ID`. Its allowed display socket
should be Wayland, and `WAYLAND_DISPLAY` should refer to a `gamescope-*` socket,
not Sway's socket. An entered shell can differ from the original app's
environment, so also inspect `/.flatpak-info` for the launch-time settings.
The gamepad option, narrow device access, lease checks, and disconnect monitor
are still required. Do not treat a passing command-profile test as a passing
compositor or Roblox test.

Status: Gamescope 3.17.0 and Soberized 0.4.0 are now installed on this host. The
first nested launch failed at `vkCreateInstance` with result `-9`
(`VK_ERROR_INCOMPATIBLE_DRIVER`). The host had the Vulkan loader but no driver
manifests, and `vulkan-radeon` was absent. A minimal host Vulkan instance probe
returned the same error independently of Gamescope. The matching package version
in Arch's local repository metadata was `1:26.2.7-1`, matching installed Mesa.

The failed launch stopped its owner and removed the neutral virtual controller
after zero pulses. After driver installation, host Vulkan enumeration succeeded
with RADV/Mesa 26.2.7, Gamescope created `gamescope-0`, and Sober launched inside
it. Sway showed only the outer Gamescope surface, while Flatpak's display
permissions allowed Wayland and disabled X11. A diagnostic at 11:38:28
Asia/Manila had a matching neutral reset, and the user confirmed visible avatar
movement with the terminal focused. The same installed 0.4.0 build then completed
a 30-minute hidden-workspace observation from 11:41:00 to 12:11:00 Asia/Manila
without a logged disconnect. Three scheduled pulses occurred during the trial,
with matching neutral resets, and the session remained armed with four total
pulses including the diagnostic.

The observer had two recorded sampling gaps, approximately 57 and 127 seconds;
Soberized's own disconnect monitor kept running. The user confirmed Roblox
stayed hidden and untouched during both gaps. See the full
[validation record](VALIDATION.md#observed-gamescope-trial-2026-11-18). This is a
successful result for this run; overnight and other-experience behavior remain
unverified.

## Temporary Flatpak process ID during startup

One launch after driver installation failed with `invalid Sober process ID`.
A fast metadata capture showed Flatpak publishing a Sober instance with PID `0`
before replacing it with the actual positive PID. A retry of 0.4.0 succeeded.

Version 0.4.1 waits through that pending state only in the bounded startup loops.
It never binds a lease target to PID zero. Duplicate-instance checks still
recognize the pending app, malformed metadata still fails, and active-session
monitoring remains strict. The startup fix is also included in 0.4.2 and later
builds.

## Camera mouse capture

The tested standard profile later showed a camera issue: menu clicks and keyboard
movement worked, but right-button dragging locked the cursor without rotating
the camera. Relaunching the standard profile did not resolve it. This is
consistent with a relative-pointer/lock forwarding problem, without identifying
the exact component responsible.

Version 0.4.2 adds an opt-in profile for testing Gamescope's explicit relative
mouse mode:

```sh
soberized launch --gamescope --capture-mouse
```

The flag must be supplied when launching, after closing the current Sober
window. It maps to Gamescope's `--force-grab-cursor` and is not enabled by
default. It may keep the pointer confined even when navigating menus. No
keyboard grab is requested; use your normal Sway workspace shortcut to leave
the window. `Super+G` toggles Gamescope's keyboard grab, not this mouse option.

Before using this profile for AFK sessions, test right-drag/first-person camera
movement, menu interaction after releasing the right button, and ordinary
desktop mouse/typing after switching workspaces. Treat a failed release as a
failed compatibility test. The earlier 30-minute success used the standard
profile and does not independently validate the capture profile.

On the target Arch/Sway host, the local **0.4.2 launcher** successfully launched
this profile with the compatible installed 0.4.0 controller/helper. Process
arguments confirmed cursor capture enabled and keyboard grabbing absent. The
user then confirmed that **camera rotation, menus, and desktop mouse use after
switching workspaces all work**. The virtual controller remained neutral until
the game join, then armed normally. No system binaries were replaced for this
test; install the built 0.4.2 release before using the flag with the installed
command. The capture profile has not yet completed its own unattended idle trial.

## Vulkan startup failure

For this AMD machine, fix the missing driver and retry the already installed
Soberized release:

```sh
sudo pacman -S --needed vulkan-radeon vulkan-tools
vulkaninfo --summary
soberized launch --gamescope
```

The `No CAP_SYS_NICE` message is a fallback to ordinary scheduling, separate
from the Vulkan failure. Soberized requests no scheduling capabilities. There
is no need to rebuild/reinstall the companion just to install the Vulkan driver.

## Primary references

- [Gamescope maintainer on internal focus](https://github.com/ValveSoftware/gamescope/issues/636#issuecomment-1264253700)
- [Native Wayland focus implementation](https://github.com/ValveSoftware/gamescope/blob/master/src/wlserver.cpp)
- [Nested display environment setup](https://github.com/ValveSoftware/gamescope/blob/master/src/main.cpp)
- [Optional Vulkan layer environment switches](https://github.com/ValveSoftware/gamescope/blob/master/layer/VkLayer_FROG_gamescope_wsi.json.in)
- [Arch Gamescope package](https://archlinux.org/packages/extra/x86_64/gamescope/)
- [Arch AMD Vulkan driver](https://archlinux.org/packages/extra/x86_64/vulkan-radeon/)
- [Vulkan result codes](https://docs.vulkan.org/refpages/latest/refpages/source/VkResult.html)
- [Flatpak display socket permissions](https://docs.flatpak.org/en/latest/sandbox-permissions.html)
