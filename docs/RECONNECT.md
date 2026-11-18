# Automatic reconnect

Automatic reconnect is experimental and opt-in. It uses Roblox's own Reconnect
button inside the existing Gamescope window. No place IDs, server links, account
cookies, or authentication tickets are required.

## Setup

Install the matching current CLI/helper using the
[update guide](INSTALLATION.md). The user-session frontend needs `libei`; dialog
recognition also needs Gamescope's control utility, Tesseract, and English data:

```sh
sudo pacman -S --needed gamescope libei tesseract tesseract-data-eng
```

With Sober closed, launch:

```sh
soberized launch --gamescope --capture-mouse --auto-reconnect
```

For Sober already running inside Gamescope:

```sh
soberized stop
soberized start --auto-reconnect
```

Ordinary `start` and `launch` keep recovery disabled. Stop before changing modes.
The preference is an owner-only runtime file, cleared by `stop` and logout/reboot.
There is no background restart policy after the session worker fails.

```sh
soberized status
soberized stop
```

`stop` cancels recovery, releases input, and leaves Sober open. The controller
remains visible to other applications during normal use and recovery; stop
before playing another game.

## Recovery behavior

Only client-reported timeouts (266), connection loss (277), and idle timeout
(278) qualify. Leaving manually, a game kick, an unknown reason, a missing or
rotated log, a changed Flatpak instance, suspend, or lease loss stops input.

On an eligible disconnect, the helper immediately pauses scheduled movement and
neutralizes its existing controller. It retains the same leased device during
recovery. Attempts wait 15, 30, then 60 seconds, with at most three attempts.
Two minutes of observed client connection state reset the retry budget; a brief
join followed by another disconnect does not reset it.

Before every click, Soberized:

1. Binds to the Gamescope ancestor of the exact Sober Flatpak instance, verifying
   local socket peer PID/UID. An unrelated Gamescope instance is rejected.
2. Captures only that compositor's game image into a private temporary directory.
3. Requires readable English `Disconnected`, `Leave`, `Reconnect`, and the
   matching `Error Code`. Reconnect must be the unique button to Leave's right.
4. Requires the same target in a second capture, and rechecks the client state.
5. Sends one absolute pointer move and one left click through that Gamescope's
   input socket. No host pointer device or global input tool is used.

The frontend requests pointer/button access only. Keyboard emulation and desktop
remote-control portals are not used. The helper's kernel device stays gamepad-only.
The existing Gamescope window stays open; the program does not relaunch Sober or
switch host workspaces.

Images and OCR output are removed after each inspection. Neither is journaled.
Abrupt termination can leave private capture files in the user runtime directory
until its cleanup at logout/reboot.
Coordinates come from the detected text, with no resolution or cursor-position
configuration. Images larger than 4096×2160 or 32 MiB, ambiguous labels, changed
layouts, low-confidence OCR, unsupported languages, and capture failures stop
recovery without a click. This recognition is a heuristic, not proof that a UI
is trustworthy.

After a click, the worker waits up to 90 seconds for a new join followed by a
fresh client network marker. Movement stays paused during this wait. A join
request alone cannot resume pulses. After recovery, the next movement pulse is
ten minutes later; missed pulses are not replayed. Client logs cannot prove that
the server remains connected or that the experience is fully loaded.

## Diagnostics and validation

Check the input handshake and dialog recognition without sending input:

```sh
soberized check-reconnect
```

With a qualifying disconnect dialog visible or hidden inside Gamescope, this
reports whether its Reconnect button was recognized. Without a logged disconnect,
it only checks the capture/OCR path. It never clicks.

For one controlled recovery attempt, leave Sober on its disconnect dialog:

```sh
soberized stop
soberized test-reconnect
```

The test waits ten seconds, recognizes the dialog, sends at most one click, and
waits for a fresh client connection marker. Ctrl+C or `soberized stop` cancels it.
It does not arm anti-idle movement; run `soberized start --auto-reconnect` afterward.
If no qualifying disconnect is currently logged, it refuses before preparing
input or clicking.

Inspect recovery decisions and input records with:

```sh
journalctl --user -u soberized-session.service -b --no-pager
journalctl -u soberized-helper.service -b --no-pager
```

On the target Sway host, the Gamescope peer binding and libei sender handshake
passed without sending input. Capture/OCR also completed on the real client and
rejected its ordinary game screen. Actual recognition of a disconnect dialog,
click delivery, and unattended reconnect still require a live trial. The normal
hidden-workspace anti-idle trial does not validate this new feature.

## Primary references

- [Gamescope input emulation](https://github.com/ValveSoftware/gamescope/blob/3.17.0/src/InputEmulation.cpp)
- [Gamescope screenshot control](https://github.com/ValveSoftware/gamescope/blob/3.17.0/src/steamcompmgr.cpp)
- [libei sender API](https://libinput.pages.freedesktop.org/libei/api/)
- [Tesseract command line](https://tesseract-ocr.github.io/tessdoc/Command-Line-Usage.html)
