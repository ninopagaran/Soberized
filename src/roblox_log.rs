//! Bounded, read-only observation of Roblox engine log events. Never execute log
//! content or retain account IDs, server addresses, tickets, or complete lines.
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const SNAPSHOT_BYTES: u64 = 8 * 1024 * 1024;
const POLL_BYTES: usize = 256 * 1024;
const MAX_LINE: usize = 8 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Joined { timestamp: String },
    Disconnected { timestamp: String, code: u16 },
}

pub fn reason(code: u16) -> &'static str {
    match code {
        278 => "idle timeout",
        277 => "connection lost",
        266 => "connection timeout",
        267 => "kicked by experience",
        285 => "left experience",
        _ => "disconnect",
    }
}

impl Event {
    pub fn disconnect_message(&self, pulses: Option<u32>) -> Option<String> {
        match self {
            Self::Disconnected { timestamp, code } => Some(format!(
                "Roblox disconnected: {} (code {}) at {}. Input stopped{}. Rejoin the experience, then run soberized start.",
                reason(*code),
                code,
                timestamp,
                pulses.map_or_else(String::new, |count| format!(" after {count} pulses"))
            )),
            Self::Joined { .. } => None,
        }
    }
}

fn timestamp_valid(value: &str) -> bool {
    let b = value.as_bytes();
    if !(20..=30).contains(&b.len()) || b.last() != Some(&b'Z') {
        return false;
    }
    for (index, expected) in [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':')] {
        if b[index] != expected {
            return false;
        }
    }
    for range in [0..4, 5..7, 8..10, 11..13, 14..16, 17..19] {
        if !b[range].iter().all(u8::is_ascii_digit) {
            return false;
        }
    }
    let fraction = &b[19..b.len() - 1];
    fraction.is_empty()
        || (fraction.len() > 1
            && fraction[0] == b'.'
            && fraction[1..].iter().all(u8::is_ascii_digit))
}

pub fn parse_event(line: &str) -> Option<Event> {
    let line = line.strip_prefix("info: Roblox: ")?;
    let mut fields = line.splitn(4, ',');
    let timestamp = fields.next()?;
    let elapsed = fields.next()?;
    let thread = fields.next()?;
    let payload = fields.next()?.trim_end_matches('\r');
    if !timestamp_valid(timestamp)
        || elapsed.is_empty()
        || elapsed.len() > 32
        || !elapsed.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        || thread.is_empty()
        || thread.len() > 16
        || !thread.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    // Engine category must be at the start of the payload. A game's Output
    // message embedding a fake NetworkClient category must not stop the lease.
    if let Some(code) = payload.strip_prefix(
        "6,Info [DFLog::NetworkClient] Client:Disconnect setting replicator disconnect reason to ",
    ).or_else(|| payload.strip_prefix("7 [FLog::Network] Sending disconnect with reason: ")) {
        if code.len() > 4 || !code.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let code = code.parse::<u16>().ok().filter(|n| *n > 0)?;
        return Some(Event::Disconnected {
            timestamp: timestamp.to_owned(),
            code,
        });
    }
    if payload.starts_with("6 [FLog::Output] ! Joining game '") {
        return Some(Event::Joined {
            timestamp: timestamp.to_owned(),
        });
    }
    None
}

pub struct Monitor {
    path: PathBuf,
    not_before: SystemTime,
    file: Option<File>,
    identity: Option<(u64, u64)>,
    partial: Vec<u8>,
    discard_line: bool,
    latest: Option<Event>,
    connected: bool,
    connection_revision: u64,
    generation: u64,
}

impl Monitor {
    pub fn attach(path: impl Into<PathBuf>, not_before: SystemTime) -> io::Result<Self> {
        let mut monitor = Self {
            path: path.into(),
            not_before,
            file: None,
            identity: None,
            partial: Vec::with_capacity(MAX_LINE),
            discard_line: false,
            latest: None,
            connected: false,
            connection_revision: 0,
            generation: 0,
        };
        monitor.poll_budget(SNAPSHOT_BYTES as usize)?;
        Ok(monitor)
    }

    pub fn latest(&self) -> Option<&Event> {
        self.latest.as_ref()
    }

    /// A client network marker, not proof of a live server or successful load.
    pub fn connection_revision(&self) -> u64 {
        self.connection_revision
    }

    pub fn connected(&self) -> bool {
        self.connected
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn poll(&mut self) -> io::Result<()> {
        self.poll_budget(POLL_BYTES)
    }

    fn poll_budget(&mut self, budget: usize) -> io::Result<()> {
        let metadata = match std::fs::metadata(&self.path) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.file = None;
                self.identity = None;
                self.latest = None;
                self.connected = false;
                self.partial.clear();
                self.discard_line = false;
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if !metadata.is_file() {
            return Err(io::Error::other("Sober log must be a regular file"));
        }
        let identity = (metadata.dev(), metadata.ino());
        let position = match self.file.as_mut() {
            Some(file) => file.stream_position()?,
            None => 0,
        };
        if self.identity != Some(identity) || metadata.len() < position {
            self.generation = self.generation.wrapping_add(1);
            // A newly launched Sober initially still has the previous app's
            // latest.log. Wait for a file modified by this process lifetime.
            if metadata.modified()? < self.not_before {
                self.file = None;
                self.identity = None;
                self.latest = None;
                self.connected = false;
                self.partial.clear();
                self.discard_line = false;
                return Ok(());
            }
            let mut file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(&self.path)?;
            let metadata = file.metadata()?;
            if !metadata.is_file() {
                return Err(io::Error::other("Sober log must be a regular file"));
            }
            self.identity = Some((metadata.dev(), metadata.ino()));
            let start = metadata.len().saturating_sub(SNAPSHOT_BYTES);
            file.seek(SeekFrom::Start(start))?;
            self.file = Some(file);
            self.latest = None;
            self.connected = false;
            self.partial.clear();
            self.discard_line = start > 0;
        }
        if self.file.is_none() {
            return Ok(());
        }
        let mut buffer = [0u8; 16 * 1024];
        let mut remaining = budget;
        while remaining > 0 {
            let length = remaining.min(buffer.len());
            let count = self.file.as_mut().unwrap().read(&mut buffer[..length])?;
            if count == 0 {
                break;
            }
            remaining -= count;
            for &byte in &buffer[..count] {
                if byte == b'\n' {
                    if !self.discard_line {
                        if let Ok(line) = std::str::from_utf8(&self.partial) {
                            // Only accept the exact network category after a
                            // validated engine header; retain no address data.
                            if let Some((header, _)) =
                                line.split_once(",7 [FLog::Network] serverId: ")
                            {
                                let synthetic = format!(
                                    "{header},7 [FLog::Network] Sending disconnect with reason: 285"
                                );
                                if matches!(
                                    parse_event(&synthetic),
                                    Some(Event::Disconnected { .. })
                                ) && matches!(self.latest, Some(Event::Joined { .. }))
                                {
                                    self.connected = true;
                                    self.connection_revision =
                                        self.connection_revision.wrapping_add(1);
                                }
                            }
                            if let Some(event) = parse_event(line) {
                                // Generic cleanup follows explicit idle/network
                                // errors; preserve that cause until a new join.
                                let cleanup =
                                    matches!(event, Event::Disconnected { code: 285, .. });
                                let has_error = matches!(self.latest.as_ref(),Some(Event::Disconnected{code,..}) if *code!=285);
                                if !cleanup || !has_error {
                                    self.latest = Some(event);
                                    self.connected = false;
                                }
                            }
                        }
                    }
                    self.partial.clear();
                    self.discard_line = false;
                } else if !self.discard_line {
                    if self.partial.len() == MAX_LINE {
                        self.partial.clear();
                        self.discard_line = true;
                    } else {
                        self.partial.push(byte);
                    }
                }
            }
        }
        Ok(())
    }
}

pub fn default_path() -> io::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| io::Error::other("HOME is unavailable; cannot locate the Sober log"))?;
    Ok(Path::new(&home).join(".var/app/org.vinegarhq.Sober/data/sober/sober_logs/latest.log"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_fresh_network_marker_is_required_after_a_new_join_and_log_rotation_changes_generation() {
        let temp = Temp::new();
        let marker =
            "info: Roblox: 2026-11-18T00:26:55Z,1,abcd,7 [FLog::Network] serverId: <redacted>\n";
        std::fs::write(temp.path(), format!("{JOIN}{marker}")).unwrap();
        let mut monitor = Monitor::attach(temp.path(), SystemTime::UNIX_EPOCH).unwrap();
        assert!(monitor.connected());
        let baseline = monitor.connection_revision();
        let generation = monitor.generation();
        let mut file = OpenOptions::new().append(true).open(temp.path()).unwrap();
        file.write_all(format!("{IDLE}{JOIN}").as_bytes()).unwrap();
        monitor.poll().unwrap();
        assert!(!monitor.connected());
        assert_eq!(monitor.connection_revision(), baseline);
        file.write_all(b"info: Roblox: 2026-11-18T00:26:55Z,1,abcd,6 [FLog::Output] ,7 [FLog::Network] serverId: <redacted>\n").unwrap();
        monitor.poll().unwrap();
        assert!(!monitor.connected());
        file.write_all(marker.as_bytes()).unwrap();
        monitor.poll().unwrap();
        assert!(monitor.connected());
        assert!(monitor.connection_revision() > baseline);
        let next = temp.0.join("next.log");
        std::fs::write(&next, format!("{JOIN}{marker}")).unwrap();
        std::fs::rename(next, temp.path()).unwrap();
        monitor.poll().unwrap();
        assert_ne!(monitor.generation(), generation);
    }
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    const JOIN: &str = "info: Roblox: 2026-11-18T00:26:54.021Z,26287.021484,cc0dc6c0,6 [FLog::Output] ! Joining game 'redacted' place 1 at redacted\n";
    const IDLE: &str = "info: Roblox: 2026-11-18T00:46:54.021Z,27487.021484,cc0dc6c0,6,Info [DFLog::NetworkClient] Client:Disconnect setting replicator disconnect reason to 278\n";
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "soberized-log-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> PathBuf {
            self.0.join("latest.log")
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reads_only_engine_events_without_private_join_data() {
        assert!(matches!(
            parse_event(JOIN.trim_end()),
            Some(Event::Joined { .. })
        ));
        let event = parse_event(IDLE.trim_end()).unwrap();
        assert_eq!(
            event,
            Event::Disconnected {
                timestamp: "2026-11-18T00:46:54.021Z".into(),
                code: 278
            }
        );
        let message = event.disconnect_message(Some(2)).unwrap();
        assert!(message.contains("idle timeout"));
        assert!(message.contains("after 2 pulses"));
        for line in [
            "Client:Disconnect setting replicator disconnect reason to 278",
            "info: Roblox: not-a-time,1,abcd,6,Info [DFLog::NetworkClient] Client:Disconnect setting replicator disconnect reason to 278",
            "info: Roblox: 2026-11-18T00:46:54Z,1,abcd,6 [FLog::Output] 6,Info [DFLog::NetworkClient] Client:Disconnect setting replicator disconnect reason to 278",
        ] {
            assert!(parse_event(line).is_none(), "{line}");
        }
    }

    #[test]
    fn snapshot_append_partial_lines_and_rejoin_are_followed() {
        let temp = Temp::new();
        std::fs::write(temp.path(), JOIN).unwrap();
        let mut monitor = Monitor::attach(temp.path(), SystemTime::UNIX_EPOCH).unwrap();
        assert!(matches!(monitor.latest(), Some(Event::Joined { .. })));
        let mut file = OpenOptions::new().append(true).open(temp.path()).unwrap();
        file.write_all(&IDLE.as_bytes()[..30]).unwrap();
        monitor.poll().unwrap();
        assert!(matches!(monitor.latest(), Some(Event::Joined { .. })));
        file.write_all(&IDLE.as_bytes()[30..]).unwrap();
        monitor.poll().unwrap();
        assert!(matches!(
            monitor.latest(),
            Some(Event::Disconnected { code: 278, .. })
        ));
        file.write_all(JOIN.as_bytes()).unwrap();
        monitor.poll().unwrap();
        assert!(matches!(monitor.latest(), Some(Event::Joined { .. })));
    }

    #[test]
    fn missing_logs_rotation_and_truncation_are_handled() {
        let temp = Temp::new();
        let mut monitor = Monitor::attach(temp.path(), SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(monitor.latest(), None);
        std::fs::write(temp.path(), format!("{JOIN}{}\n", "x".repeat(5000))).unwrap();
        monitor.poll().unwrap();
        assert!(matches!(monitor.latest(), Some(Event::Joined { .. })));
        std::fs::write(temp.path(), IDLE).unwrap();
        monitor.poll().unwrap();
        assert!(matches!(monitor.latest(), Some(Event::Disconnected { .. })));
        let next = temp.0.join("rotated.log");
        std::fs::write(&next, JOIN).unwrap();
        std::fs::rename(next, temp.path()).unwrap();
        monitor.poll().unwrap();
        assert!(matches!(monitor.latest(), Some(Event::Joined { .. })));
    }

    #[test]
    fn oversized_and_invalid_lines_do_not_hide_the_next_engine_event() {
        let temp = Temp::new();
        let mut content = vec![b'x'; MAX_LINE * 3];
        content.push(b'\n');
        content.extend_from_slice(&[255, b'\n']);
        content.extend_from_slice(IDLE.as_bytes());
        std::fs::write(temp.path(), content).unwrap();
        let monitor = Monitor::attach(temp.path(), SystemTime::UNIX_EPOCH).unwrap();
        assert!(matches!(
            monitor.latest(),
            Some(Event::Disconnected { code: 278, .. })
        ));
    }

    #[test]
    fn previous_process_logs_are_ignored() {
        let temp = Temp::new();
        std::fs::write(temp.path(), IDLE).unwrap();
        let future = SystemTime::now() + std::time::Duration::from_secs(60);
        let monitor = Monitor::attach(temp.path(), future).unwrap();
        assert_eq!(monitor.latest(), None);
    }

    #[test]
    fn cleanup_preserves_idle_reason_until_a_later_game_join() {
        let temp = Temp::new();
        let cleanup = "info: Roblox: 2026-11-18T00:46:55Z,1,abcd,7 [FLog::Network] Sending disconnect with reason: 285\n";
        std::fs::write(temp.path(), format!("{JOIN}{IDLE}{cleanup}")).unwrap();
        let mut monitor = Monitor::attach(temp.path(), SystemTime::UNIX_EPOCH).unwrap();
        assert!(matches!(
            monitor.latest(),
            Some(Event::Disconnected { code: 278, .. })
        ));
        let mut file = OpenOptions::new().append(true).open(temp.path()).unwrap();
        file.write_all(JOIN.as_bytes()).unwrap();
        file.write_all(cleanup.as_bytes()).unwrap();
        monitor.poll().unwrap();
        assert!(matches!(
            monitor.latest(),
            Some(Event::Disconnected { code: 285, .. })
        ));
    }

    #[test]
    fn large_appends_are_consumed_over_bounded_polls() {
        let temp = Temp::new();
        std::fs::write(temp.path(), JOIN).unwrap();
        let mut monitor = Monitor::attach(temp.path(), SystemTime::UNIX_EPOCH).unwrap();
        let mut file = OpenOptions::new().append(true).open(temp.path()).unwrap();
        file.write_all(&vec![b'x'; POLL_BYTES + 100]).unwrap();
        file.write_all(b"\n").unwrap();
        file.write_all(IDLE.as_bytes()).unwrap();
        monitor.poll().unwrap();
        assert!(matches!(monitor.latest(), Some(Event::Joined { .. })));
        monitor.poll().unwrap();
        assert!(matches!(
            monitor.latest(),
            Some(Event::Disconnected { code: 278, .. })
        ));
    }
}
