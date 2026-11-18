//! Fixed launch profiles. No shell, arbitrary application, or extra arguments.
use crate::APP_ID;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

pub const GAMESCOPE_PATH: &str = "/usr/bin/gamescope";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Native,
    Gamescope,
}

impl Mode {
    pub fn parse(options: &[String]) -> io::Result<Self> {
        match options {
            [] => Ok(Self::Native),
            [option] if option == "--gamescope" => Ok(Self::Gamescope),
            _ => Err(io::Error::other(
                "usage: soberized launch [--gamescope]; extra launch arguments are not supported",
            )),
        }
    }

    pub fn check_requirements(self) -> io::Result<()> {
        if self == Self::Gamescope {
            check_gamescope(
                Path::new(GAMESCOPE_PATH),
                std::env::var_os("WAYLAND_DISPLAY").as_deref(),
            )?;
        }
        Ok(())
    }

    pub fn command(self) -> Command {
        self.command_with_paths(Path::new(GAMESCOPE_PATH), Path::new("/usr/bin/flatpak"))
    }

    fn command_with_paths(self, gamescope: &Path, flatpak: &Path) -> Command {
        let mut command = if self == Self::Gamescope {
            let mut command = Command::new(gamescope);
            command.args([
                "--backend",
                "wayland",
                "--expose-wayland",
                "-w",
                "1280",
                "-h",
                "720",
                "-r",
                "30",
                "-o",
                "30",
                "--",
            ]);
            command.arg(flatpak);
            // Keep the compositor outside the terminal's foreground job group.
            // Do not request keyboard/cursor grabs, DRM control, or capabilities.
            command.process_group(0);
            command
        } else {
            Command::new(flatpak)
        };
        command.args([
            "run",
            "--device=input",
            "--env=SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS=1",
        ]);
        if self == Self::Gamescope {
            // With --expose-wayland Gamescope sets WAYLAND_DISPLAY to its own
            // socket before executing Flatpak. Keep Sober on that native path.
            command.args([
                "--socket=wayland",
                "--nosocket=x11",
                "--nosocket=fallback-x11",
                "--unset-env=DISPLAY",
                "--env=SDL_VIDEODRIVER=wayland",
                "--env=SDL_VIDEO_DRIVER=wayland",
                "--unset-env=ENABLE_GAMESCOPE_WSI",
                "--env=DISABLE_GAMESCOPE_WSI=1",
            ]);
        }
        command.arg(APP_ID);
        command
    }
}

pub fn gamescope_installed() -> bool {
    executable(Path::new(GAMESCOPE_PATH))
}

fn executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn check_gamescope(path: &Path, wayland_display: Option<&std::ffi::OsStr>) -> io::Result<()> {
    if !executable(path) {
        return Err(io::Error::other(
            "Gamescope is not installed; on Arch install it with sudo pacman -S --needed gamescope, then retry soberized launch --gamescope",
        ));
    }
    if wayland_display.is_none_or(|display| display.is_empty()) {
        return Err(io::Error::other(
            "Gamescope mode requires an active Wayland desktop with WAYLAND_DISPLAY set",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn launch_options_reject_arbitrary_programs_and_passthrough_arguments() {
        assert_eq!(Mode::parse(&[]).unwrap(), Mode::Native);
        assert_eq!(
            Mode::parse(&["--gamescope".into()]).unwrap(),
            Mode::Gamescope
        );
        for arguments in [
            vec!["--gamescope", "--device=all"],
            vec!["--gamescope", "--", "/bin/sh"],
            vec!["--gamescope", "--grab"],
            vec!["--gamescope=anything"],
            vec!["--command=/bin/sh"],
        ] {
            let arguments = arguments.into_iter().map(String::from).collect::<Vec<_>>();
            assert!(Mode::parse(&arguments).is_err());
        }
    }

    #[test]
    fn unavailable_compositor_and_missing_desktop_fail_preflight() {
        assert!(check_gamescope(Path::new("/dev/null"), Some(OsStr::new("wayland-0"))).is_err());
        assert!(check_gamescope(Path::new("/bin/sh"), None).is_err());
        assert!(check_gamescope(Path::new("/bin/sh"), Some(OsStr::new(""))).is_err());
        assert!(check_gamescope(Path::new("/bin/sh"), Some(OsStr::new("wayland-0"))).is_ok());
    }

    #[test]
    fn wrapped_child_receives_the_nested_display_and_bounded_flatpak_profile() {
        // Exercise process/argument boundaries with harmless stand-ins, without
        // a compositor, Flatpak sandbox, systemd service, or input device.
        let directory =
            std::env::temp_dir().join(format!("soberized-launch-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        let gamescope = directory.join("gamescope");
        let flatpak = directory.join("flatpak");
        std::fs::write(
            &gamescope,
            "#!/bin/sh\nset -eu\nwhile [ \"$1\" != -- ]; do shift; done\nshift\nDISPLAY=:99\nWAYLAND_DISPLAY=gamescope-test\nexport DISPLAY WAYLAND_DISPLAY\nexec \"$@\"\n",
        ).unwrap();
        std::fs::write(
            &flatpak,
            "#!/bin/sh\nprintf 'display=%s\\nwayland=%s\\n' \"$DISPLAY\" \"${WAYLAND_DISPLAY-unset}\"\nprintf '%s\\n' \"$@\"\n",
        ).unwrap();
        for path in [&gamescope, &flatpak] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        for mode in [Mode::Native, Mode::Gamescope] {
            let output = mode
                .command_with_paths(&gamescope, &flatpak)
                .env("DISPLAY", ":0")
                .env("WAYLAND_DISPLAY", "wayland-parent")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let text = String::from_utf8(output.stdout).unwrap();
            let lines = text.lines().collect::<Vec<_>>();
            assert_eq!(lines.last(), Some(&APP_ID));
            assert!(lines.contains(&"--device=input"));
            assert!(!lines.contains(&"--device=all"));
            assert!(!lines.iter().any(|line| line.starts_with("--filesystem=")));
            if mode == Mode::Gamescope {
                assert_eq!(&lines[..2], &["display=:99", "wayland=gamescope-test"]);
                assert!(lines.contains(&"--socket=wayland"));
                assert!(lines.contains(&"--nosocket=x11"));
                assert!(lines.contains(&"--unset-env=DISPLAY"));
                assert!(!lines.contains(&"--socket=x11"));
                assert!(lines.contains(&"--env=DISABLE_GAMESCOPE_WSI=1"));
            } else {
                assert_eq!(&lines[..2], &["display=:0", "wayland=wayland-parent"]);
                assert!(!lines.iter().any(|line| line.starts_with("--socket=")));
            }
        }
    }
}
