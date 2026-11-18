//! Bounded recovery of an observed disconnect. No credentials, links, or IDs.
use crate::gamescope_input::{Endpoint, runtime_dir};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const JOIN_WAIT_MS: u64 = 90_000;
const MARKER: &str = "soberized-auto-reconnect";
const VALUE: &[u8] = b"gamescope-dialog-v1\n";

pub fn recoverable(code: u16) -> bool {
    matches!(code, 266 | 277 | 278)
}

pub fn dependencies() -> io::Result<()> {
    for path in ["/usr/bin/gamescopectl", "/usr/bin/tesseract"] {
        let m = std::fs::metadata(path).map_err(|_| {
            io::Error::other("auto-reconnect requires gamescope, tesseract, and tesseract-data-eng")
        })?;
        if !m.is_file() || m.mode() & 0o111 == 0 {
            return Err(io::Error::other(
                "auto-reconnect dependencies are not executable",
            ));
        }
    }
    Ok(())
}

pub fn enabled() -> io::Result<bool> {
    let path = runtime_dir()?.join(MARKER);
    // SAFETY: geteuid has no preconditions.
    read_preference(&path, unsafe { libc::geteuid() })
}

fn read_preference(path: &Path, uid: u32) -> io::Result<bool> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err(io::Error::other("invalid reconnect preference permissions"));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file).take(64).read_to_end(&mut bytes)?;
    if bytes != VALUE {
        return Err(io::Error::other("invalid reconnect preference"));
    }
    Ok(true)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Observation {
    Disconnected,
    Joining,
    Reconnected,
}

pub fn observe(
    event: Option<&crate::roblox_log::Event>,
    connected: bool,
    revision: u64,
    generation: u64,
    baseline: (u64, u64),
) -> io::Result<Observation> {
    if generation != baseline.1 {
        return Err(io::Error::other(
            "Sober log rotated or truncated during recovery; no further input sent",
        ));
    }
    match event {
        None => Err(io::Error::other(
            "Sober log disappeared; recovery cancelled",
        )),
        Some(crate::roblox_log::Event::Disconnected { code, .. }) if recoverable(*code) => {
            Ok(Observation::Disconnected)
        }
        Some(crate::roblox_log::Event::Disconnected { .. }) => {
            Err(io::Error::other("disconnect reason requires manual action"))
        }
        Some(crate::roblox_log::Event::Joined { .. }) if connected && revision > baseline.0 => {
            Ok(Observation::Reconnected)
        }
        _ => Ok(Observation::Joining),
    }
}

pub fn set_enabled(enabled: bool) -> io::Result<()> {
    let path = runtime_dir()?.join(MARKER);
    if !enabled {
        return match std::fs::remove_file(path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
    }
    let scratch = Scratch::new()?;
    let temporary = scratch.0.join("preference");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(VALUE)?;
    std::fs::rename(temporary, path)
}

#[derive(Default)]
pub struct RetryBudget {
    attempts: usize,
    stable_since: Option<u64>,
}

impl RetryBudget {
    pub fn next_delay(&mut self) -> io::Result<u64> {
        self.stable_since = None;
        let delay = [15_000, 30_000, 60_000]
            .get(self.attempts)
            .copied()
            .ok_or_else(|| {
                io::Error::other("auto-reconnect exhausted three attempts; input stopped")
            })?;
        self.attempts += 1;
        Ok(delay)
    }

    pub fn attempt(&self) -> usize {
        self.attempts
    }

    pub fn observe(&mut self, now: u64, connected: bool) {
        if connected {
            let since = *self.stable_since.get_or_insert(now);
            if now.saturating_sub(since) >= 120_000 {
                self.attempts = 0;
            }
        } else {
            self.stable_since = None;
        }
    }
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> io::Result<Self> {
        let template = runtime_dir()?.join("soberized-capture-XXXXXX");
        let mut bytes = template.as_os_str().as_bytes().to_vec();
        bytes.push(0);
        // SAFETY: owned, mutable, NUL-terminated mkdtemp template. The libc
        // routine creates a unique 0700 directory, without following a link.
        if unsafe { libc::mkdtemp(bytes.as_mut_ptr().cast()) }.is_null() {
            return Err(io::Error::last_os_error());
        }
        bytes.pop();
        use std::os::unix::ffi::OsStringExt;
        Ok(Self(std::ffi::OsString::from_vec(bytes).into()))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        for name in ["frame.png", "ocr.tsv", "preference"] {
            let _ = std::fs::remove_file(self.0.join(name));
        }
        let _ = std::fs::remove_dir(&self.0);
    }
}

fn run_bounded(command: &mut Command, timeout: Duration) -> io::Result<()> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(io::Error::other(
                    "dialog capture or OCR failed; check Gamescope and the English Tesseract language data",
                ))
            };
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other(
                "dialog capture or OCR timed out; no click sent",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Button {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
impl Button {
    pub fn center(&self) -> (u32, u32) {
        (self.x + self.width / 2, self.y + self.height / 2)
    }
}

pub fn find_button(endpoint: &Endpoint, code: u16) -> io::Result<Button> {
    if !recoverable(code) {
        return Err(io::Error::other(
            "this disconnect reason is not eligible for automatic recovery",
        ));
    }
    let scratch = Scratch::new()?;
    let frame = scratch.0.join("frame.png");
    run_bounded(
        &mut endpoint.capture_command(&frame)?,
        Duration::from_millis(500),
    )?;
    let deadline = Instant::now() + Duration::from_millis(700);
    // Gamescope writes the image asynchronously after acknowledging the command.
    let dimensions = loop {
        if let Ok(dimensions) = png_dimensions(&frame) {
            break dimensions;
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other(
                "Gamescope did not produce a bounded screenshot; no click sent",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = scratch.0.join("ocr");
    let mut command = Command::new("/usr/bin/tesseract");
    command
        .arg(&frame)
        .arg(&output)
        .args(["-l", "eng", "--psm", "11", "tsv"]);
    run_bounded(&mut command, Duration::from_millis(1800))?;
    let mut file = File::open(scratch.0.join("ocr.tsv"))?;
    if file.metadata()?.len() > 512 * 1024 {
        return Err(io::Error::other("OCR output exceeded the limit"));
    }
    let mut tsv = String::new();
    file.read_to_string(&mut tsv)?;
    detect_button(&tsv, dimensions.0, dimensions.1, code)
}

fn png_dimensions(path: &Path) -> io::Result<(u32, u32)> {
    let mut file = File::open(path)?;
    if file.metadata()?.len() > 32 * 1024 * 1024 {
        return Err(io::Error::other("screenshot is too large"));
    }
    let mut header = [0u8; 24];
    file.read_exact(&mut header)?;
    if &header[..8] != b"\x89PNG\r\n\x1a\n" || &header[12..16] != b"IHDR" {
        return Err(io::Error::other("invalid screenshot"));
    }
    let width = u32::from_be_bytes(header[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(header[20..24].try_into().unwrap());
    if width == 0 || height == 0 || width > 4096 || height > 2160 {
        return Err(io::Error::other(
            "screenshot dimensions exceed reconnect limits",
        ));
    }
    // Do not hand an image still being written by Gamescope to the decoder.
    let mut tail = [0u8; 12];
    file.seek(SeekFrom::End(-12))?;
    file.read_exact(&mut tail)?;
    if &tail[4..8] != b"IEND" {
        return Err(io::Error::other("incomplete screenshot"));
    }
    Ok((width, height))
}

pub fn detect_button(tsv: &str, width: u32, height: u32, code: u16) -> io::Result<Button> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "matching English disconnect dialog was not identified; no click sent",
        )
    };
    if !recoverable(code) || width == 0 || height == 0 || width > 4096 || height > 2160 {
        return Err(invalid());
    }
    let mut reconnect = Vec::new();
    let mut leave = Vec::new();
    let mut headings = Vec::new();
    let mut text = String::new();
    for line in tsv.lines().skip(1) {
        if line.len() > 2048 {
            return Err(invalid());
        }
        let fields: Vec<_> = line.splitn(12, '\t').collect();
        if fields.len() != 12 || fields[0] != "5" {
            continue;
        }
        let confidence: f32 = fields[10].parse().map_err(|_| invalid())?;
        if !confidence.is_finite() || confidence < 75.0 {
            continue;
        }
        let normalized: String = fields[11]
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .flat_map(char::to_lowercase)
            .collect();
        text.push_str(&normalized);
        if text.len() > 64 * 1024 {
            return Err(invalid());
        }
        if !matches!(normalized.as_str(), "reconnect" | "leave" | "disconnected") {
            continue;
        }
        let n: Vec<u32> = fields[6..10]
            .iter()
            .map(|s| s.parse().map_err(|_| invalid()))
            .collect::<io::Result<_>>()?;
        let button = Button {
            x: n[0],
            y: n[1],
            width: n[2],
            height: n[3],
        };
        if button.width == 0
            || button.height == 0
            || button.x.saturating_add(button.width) > width
            || button.y.saturating_add(button.height) > height
        {
            return Err(invalid());
        }
        match normalized.as_str() {
            "reconnect" => reconnect.push(button),
            "leave" => leave.push(button),
            _ => headings.push(button),
        }
    }
    if reconnect.len() != 1
        || leave.len() != 1
        || headings.len() != 1
        || !text.contains(&format!("errorcode{code}"))
    {
        return Err(invalid());
    }
    let target = reconnect.remove(0);
    let left = &leave[0];
    let heading = &headings[0];
    // The expected two-button dialog: Leave on the left, Reconnect on the
    // same row, both below Disconnected. Ambiguous layouts fail closed.
    if target.x <= left.x
        || target.y.abs_diff(left.y) > target.height.max(left.height)
        || heading.y.saturating_add(heading.height) >= target.y
        || target.y <= height / 4
        || target.y >= height * 9 / 10
    {
        return Err(invalid());
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_requires_a_fresh_marker_and_rejects_manual_leave_or_log_replay() {
        use crate::roblox_log::Event;
        let joined = Event::Joined {
            timestamp: "redacted".into(),
        };
        assert_eq!(
            observe(Some(&joined), false, 4, 1, (4, 1)).unwrap(),
            Observation::Joining
        );
        assert_eq!(
            observe(Some(&joined), true, 4, 1, (4, 1)).unwrap(),
            Observation::Joining
        );
        assert_eq!(
            observe(Some(&joined), true, 5, 1, (4, 1)).unwrap(),
            Observation::Reconnected
        );
        assert!(observe(Some(&joined), true, 5, 2, (4, 1)).is_err());
        assert!(observe(None, false, 4, 1, (4, 1)).is_err());
        for code in [266, 277, 278, 267, 285, 268, 999] {
            let event = Event::Disconnected {
                timestamp: "redacted".into(),
                code,
            };
            let observed = observe(Some(&event), true, 5, 1, (4, 1));
            if recoverable(code) {
                assert_eq!(observed.unwrap(), Observation::Disconnected);
            } else {
                assert!(observed.is_err());
            }
        }
    }

    #[test]
    fn preferences_reject_symlinks_foreign_owners_and_unexpected_contents() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp =
            std::env::temp_dir().join(format!("soberized-preference-test-{}", std::process::id()));
        std::fs::create_dir(&temp).unwrap();
        let path = temp.join("preference");
        let uid = unsafe { libc::geteuid() };
        assert!(!read_preference(&path, uid).unwrap());
        std::fs::write(&path, VALUE).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(read_preference(&path, uid).unwrap());
        assert!(read_preference(&path, uid + 1).is_err());
        let link = temp.join("link");
        symlink(&path, &link).unwrap();
        assert!(read_preference(&link, uid).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_preference(&path, uid).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&path, b"LD_PRELOAD=malicious\n").unwrap();
        assert!(read_preference(&path, uid).is_err());
        std::fs::remove_file(link).unwrap();
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(temp).unwrap();
    }
    fn fixture() -> String {
        let mut rows = String::from(
            "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n",
        );
        for (x, y, w, h, word) in [
            (290, 200, 180, 30, "Disconnected"),
            (280, 260, 40, 20, "(Error"),
            (330, 260, 40, 20, "Code:"),
            (375, 260, 40, 20, "277)"),
            (280, 340, 50, 20, "Leave"),
            (440, 340, 100, 20, "Reconnect"),
        ] {
            rows.push_str(&format!(
                "5\t1\t1\t1\t1\t1\t{x}\t{y}\t{w}\t{h}\t95\t{word}\n"
            ));
        }
        rows
    }
    #[test]
    fn detector_requires_matching_reason_labels_and_unambiguous_layout() {
        let tsv = fixture();
        assert_eq!(
            detect_button(&tsv, 800, 600, 277).unwrap().center(),
            (490, 350)
        );
        for bad in [
            tsv.replace("Reconnect", "Resume"),
            tsv.replace("Disconnected", "Shop"),
            tsv.replace("Leave", "Buy"),
            tsv.replace("277)", "278)"),
            tsv.replace("\t440\t340\t", "\t200\t340\t"),
            tsv.replace("\t440\t340\t", "\t440\t480\t"),
            tsv.replace("\t95\tReconnect", "\t30\tReconnect"),
            tsv.replace("\t440\t340\t100\t20\t", "\t790\t340\t100\t20\t"),
            format!("{tsv}5\t1\t1\t1\t1\t1\t440\t340\t100\t20\t95\tReconnect\n"),
        ] {
            assert!(detect_button(&bad, 800, 600, 277).is_err());
        }
        assert!(detect_button(&tsv, 800, 600, 267).is_err());
    }

    #[test]
    fn retries_are_bounded_and_short_joins_do_not_reset_the_budget() {
        let mut budget = RetryBudget::default();
        assert_eq!(budget.next_delay().unwrap(), 15_000);
        budget.observe(0, true);
        budget.observe(119_999, true);
        assert_eq!(budget.next_delay().unwrap(), 30_000);
        budget.observe(120_000, false);
        assert_eq!(budget.next_delay().unwrap(), 60_000);
        assert!(budget.next_delay().is_err());
        budget.observe(200_000, true);
        budget.observe(320_000, true);
        assert_eq!(budget.next_delay().unwrap(), 15_000);
        for code in [267, 285, 268, 0, 999] {
            assert!(!recoverable(code));
        }
    }
}
