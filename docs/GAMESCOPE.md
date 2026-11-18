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

Close the current Sober window completely, then run:

```sh
soberized stop
sudo pacman -S --needed gamescope
sudo ./scripts/install.sh --user nino
systemctl --user daemon-reload
soberized --version
soberized doctor --sandbox-test
soberized launch --gamescope
```

The installed version must be `soberized 0.4.0`. Both root-owned binaries must
be updated; a successful build alone does not update installed code. Enable
gamepad support in Sober Settings before launching.

The virtual controller is prepared first and stays neutral until a game join.
Gamescope creates a 1280×720 display with a 30 FPS limit both while focused and
unfocused. Sober uses Gamescope's native Wayland socket, with X11/fallback-X11
access disabled, `DISPLAY` unset, and SDL's Wayland backend selected for that
launch. The optional Gamescope Vulkan WSI layer is disabled. No cursor/keyboard
grab or real-time privilege is requested.

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

Status: Gamescope is not installed on the development host yet. The launch
profile can be checked with harmless stand-in programs, but compositor startup,
native Wayland focus behavior, background input, and idle prevention require
the host tests above. No successful idle result is claimed.

## Primary references

- [Gamescope maintainer on internal focus](https://github.com/ValveSoftware/gamescope/issues/636#issuecomment-1264253700)
- [Native Wayland focus implementation](https://github.com/ValveSoftware/gamescope/blob/master/src/wlserver.cpp)
- [Nested display environment setup](https://github.com/ValveSoftware/gamescope/blob/master/src/main.cpp)
- [Optional Vulkan layer environment switches](https://github.com/ValveSoftware/gamescope/blob/master/layer/VkLayer_FROG_gamescope_wsi.json.in)
- [Arch Gamescope package](https://archlinux.org/packages/extra/x86_64/gamescope/)
- [Flatpak display socket permissions](https://docs.flatpak.org/en/latest/sandbox-permissions.html)
