# UI navigation diagnostic

`soberized test-ui-navigation` tests whether Roblox accepts a controller Select
press to enter UI selection. It does not reconnect or activate a highlighted
button. Automatic reconnect uses a separate
[Gamescope dialog-recognition path](RECONNECT.md).

## Run the test

Install the current CLI and helper together using the
[update instructions](INSTALLATION.md). Install the matching CLI and helper.
Keep Sober running, enable its gamepad permission, and enable Roblox's
**Settings → UI Navigation Toggle**. Then run:

```sh
soberized stop
soberized test-ui-navigation
```

Focus the Gamescope window during the ten-second delay. The virtual controller
presses Select for 100 ms, releases all controls, stays present and neutral for
another ten seconds, then is removed. No movement, direction, confirmation,
keyboard, or mouse events are sent. Ctrl+C or `soberized stop` cancels the test.
Keep other games closed: the virtual gamepad is visible to other applications.

Check whether an outline or another selection indicator appears. Test separately
inside an experience and on a disconnect dialog. The setting only enables the
backslash shortcut; it does not guarantee that every dialog supports selection.
Select toggles the selection state, so a press may exit an already active mode.
If the first test shows nothing, let it finish and repeat once to check both
toggle states.
If a highlight appears, use your physical arrow keys to see whether Reconnect can
be selected. Confirm it manually only when you can identify that button.

If no indicator appears, do not guess a direction or confirmation sequence.
The helper journal can establish whether the fixed Select press and neutral reset
were sent; it cannot establish that Roblox consumed them:

```sh
journalctl -u soberized-helper.service -b --since '5 minutes ago' --no-pager
```

Look for `diagnostic=ui-select phase=press button=BTN_SELECT`, followed by a
`phase=neutral` record approximately 100 ms later. These do not count as movement
pulses. The helper polls every 50 ms; scheduling may delay release. Owner loss,
lease expiry, suspend, stop, and process exit retain their usual fail-closed
behavior. Only one Select diagnostic is accepted per helper lifetime, and only
from a prepared, unarmed owner. Arming is rejected while the press is queued or
active.

UI navigation can remain enabled in Roblox after the test. If movement keys
navigate the UI, press backslash once to exit UI selection before resuming play
or anti-idle movement.

## Host findings

On 2026-11-18, the target host had Sober gamepad permission enabled,
`touch_mode="off"`, desktop UI enabled, and Roblox's saved
`UiNavigationKeyBindEnabled=true`. The user reported no visible highlight after
using the keyboard shortcut both inside an experience and on the disconnect
dialog. The user subsequently observed a highlight with the controller Select
test, but reported no Leave/Reconnect selection on the disconnect dialog. The
journal recorded two 100 ms Select presses after a code 277 disconnect, with no
intervening join. The user confirmed that manually clicking Reconnect works.
These observations do not establish a Gamescope or Sober bug; they show that
selection navigation has not been validated for the target dialog.

## References

- [Roblox UI Navigation Toggle](https://en.help.roblox.com/hc/en-us/articles/203314230-In-experience-Settings-and-Help)
- [Roblox's default keyboard and gamepad bindings](https://github.com/Roblox/creator-docs/blob/main/content/en-us/includes/default-bindings.md)
