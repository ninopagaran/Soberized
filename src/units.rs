//! Read-only service diagnostics. Never start privileged services or prompt for authentication.
use crate::{HELPER_UNIT, SOCKET_PATH};
use std::io;
use std::process::{Command, Stdio};

pub const SOCKET_UNIT: &str = "soberized-helper.socket";

#[derive(Debug)]
pub struct UnitStatus {
    pub load: String,
    pub active: String,
    pub result: String,
    pub status: String,
}

impl UnitStatus {
    pub fn parse(text: &str) -> io::Result<Self> {
        let mut status = Self {
            load: String::new(),
            active: String::new(),
            result: String::new(),
            status: String::new(),
        };
        for line in text.lines() {
            if let Some((name, value)) = line.split_once('=') {
                match name {
                    "LoadState" => status.load = value.to_owned(),
                    "ActiveState" => status.active = value.to_owned(),
                    "Result" => status.result = value.to_owned(),
                    "StatusText" => status.status = value.to_owned(),
                    _ => {}
                }
            }
        }
        if status.load.is_empty() || status.active.is_empty() {
            return Err(io::Error::other(
                "systemctl did not report the unit state; check your systemd connection",
            ));
        }
        Ok(status)
    }

    pub fn failed(&self) -> bool {
        self.active == "failed"
    }
}

pub fn query(user: bool, unit: &str) -> io::Result<UnitStatus> {
    let mut command = Command::new("/usr/bin/systemctl");
    if user {
        command.arg("--user");
    }
    let output = command
        .args([
            "show",
            "--no-pager",
            unit,
            "--property=LoadState",
            "--property=ActiveState",
            "--property=Result",
            "--property=StatusText",
        ])
        .stdin(Stdio::null())
        .output()?;
    let text = std::str::from_utf8(&output.stdout)
        .map_err(|_| io::Error::other("invalid systemctl output"))?;
    let status = UnitStatus::parse(text)?;
    // systemctl can return nonzero for an explicitly reported not-found unit.
    if !output.status.success() && status.load != "not-found" {
        return Err(io::Error::other(format!(
            "could not inspect {unit}; check your systemd connection"
        )));
    }
    Ok(status)
}

pub fn backend_problem(
    helper: &UnitStatus,
    socket: &UnitStatus,
    node_exists: bool,
) -> Option<String> {
    for (unit, status) in [(HELPER_UNIT, helper), (SOCKET_UNIT, socket)] {
        if status.failed() {
            return Some(format!(
                "{unit} failed ({}); inspect journalctl -u {HELPER_UNIT} -b. After fixing the cause, run sudo systemctl reset-failed {HELPER_UNIT} {SOCKET_UNIT}, then sudo systemctl restart {SOCKET_UNIT}",
                if status.result.is_empty() {
                    "unknown result"
                } else {
                    &status.result
                }
            ));
        }
    }
    if socket.load == "not-found" || helper.load == "not-found" {
        return Some("helper units are not installed; run the Soberized installer".to_owned());
    }
    if socket.active != "active" {
        return Some(format!(
            "helper socket is {}; run sudo systemctl enable --now {SOCKET_UNIT}",
            socket.active
        ));
    }
    if !node_exists {
        return Some(format!(
            "helper socket is active but {SOCKET_PATH} is missing; run sudo systemctl restart {SOCKET_UNIT}"
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state(active: &str, result: &str) -> UnitStatus {
        UnitStatus::parse(&format!(
            "LoadState=loaded\nActiveState={active}\nResult={result}\n"
        ))
        .unwrap()
    }

    #[test]
    fn a_clean_session_exit_keeps_the_backend_ready() {
        assert!(
            backend_problem(
                &state("inactive", "success"),
                &state("active", "success"),
                true
            )
            .is_none()
        );
    }

    #[test]
    fn failed_backend_is_not_reported_as_stopped_or_uninstalled() {
        for (helper, socket) in [
            (
                state("failed", "start-limit-hit"),
                state("failed", "service-start-limit-hit"),
            ),
            (
                state("inactive", "success"),
                state("failed", "service-start-limit-hit"),
            ),
        ] {
            let message = backend_problem(&helper, &socket, false).unwrap();
            assert!(message.contains("failed"));
            assert!(message.contains("start-limit-hit"));
            assert!(message.contains("reset-failed"));
            assert!(!message.contains("not installed"));
        }
    }

    #[test]
    fn inactive_and_missing_socket_have_specific_recovery_actions() {
        let helper = state("inactive", "success");
        assert!(
            backend_problem(&helper, &state("inactive", "success"), false)
                .unwrap()
                .contains("enable --now")
        );
        assert!(
            backend_problem(&helper, &state("active", "success"), false)
                .unwrap()
                .contains("restart")
        );
        let absent = UnitStatus::parse("LoadState=not-found\nActiveState=inactive\n").unwrap();
        assert!(
            backend_problem(&absent, &absent, false)
                .unwrap()
                .contains("not installed")
        );
    }

    #[test]
    fn unavailable_systemd_information_is_not_treated_as_inactive() {
        for text in [
            "",
            "permission denied",
            "ActiveState=inactive",
            "LoadState=loaded",
        ] {
            assert!(UnitStatus::parse(text).is_err());
        }
    }
}
