use crate::APP_ID;
use std::io;
use std::process::Command;
use std::time::{Duration, SystemTime};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instance {
    pub id: String,
    pub pid: u32,
}

pub fn parse_instances(text: &str) -> io::Result<Option<Instance>> {
    let mut found = None;
    for line in text.lines() {
        let columns: Vec<_> = line.split_whitespace().collect();
        if columns.len() < 2 || columns[1] != APP_ID {
            continue;
        }
        if columns.len() != 3
            || columns[0].is_empty()
            || !columns[0].bytes().all(|b| b.is_ascii_digit())
        {
            return Err(io::Error::other("unexpected Sober instance metadata"));
        }
        let pid = columns[2]
            .parse::<u32>()
            .ok()
            .filter(|pid| *pid > 0)
            .ok_or_else(|| io::Error::other("invalid Sober process ID"))?;
        if found.is_some() {
            return Err(io::Error::other(
                "multiple Sober instances are running; keep one instance open",
            ));
        }
        found = Some(Instance {
            id: columns[0].to_owned(),
            pid,
        });
    }
    Ok(found)
}

pub fn instance() -> io::Result<Option<Instance>> {
    let output = Command::new("/usr/bin/flatpak")
        .args(["ps", "--columns=instance,application,pid"])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(
            "could not inspect running Flatpak applications",
        ));
    }
    let text = std::str::from_utf8(&output.stdout)
        .map_err(|_| io::Error::other("invalid Flatpak metadata encoding"))?;
    parse_instances(text)
}

pub fn started_at(instance: &Instance) -> io::Result<SystemTime> {
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", instance.pid))?;
    let fields = stat
        .rsplit_once(')')
        .ok_or_else(|| io::Error::other("invalid Sober process metadata"))?
        .1;
    let ticks = fields
        .split_whitespace()
        .nth(19)
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| io::Error::other("invalid Sober process start time"))?;
    // SAFETY: sysconf reads a system constant and has no pointer arguments.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if hz <= 0 {
        return Err(io::Error::other(
            "could not obtain the process clock frequency",
        ));
    }
    let started_ms = ticks.saturating_mul(1000) / (hz as u64);
    let age = crate::helper::boot_ms()?.saturating_sub(started_ms);
    SystemTime::now()
        .checked_sub(Duration::from_millis(age))
        .ok_or_else(|| io::Error::other("invalid process lifetime"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_only_one_exact_sober_application() {
        assert_eq!(
            parse_instances("42\torg.vinegarhq.Sober\t1234\n").unwrap(),
            Some(Instance {
                id: "42".into(),
                pid: 1234
            })
        );
        assert_eq!(
            parse_instances("42 org.vinegarhq.Sober.other 1234\n").unwrap(),
            None
        );
        assert!(parse_instances("42 org.vinegarhq.Sober 12\n43 org.vinegarhq.Sober 13").is_err());
        for line in [
            "42 org.vinegarhq.Sober x",
            "42 org.vinegarhq.Sober 0",
            "x org.vinegarhq.Sober 12",
            "42 org.vinegarhq.Sober",
        ] {
            assert!(parse_instances(line).is_err());
        }
    }
}
