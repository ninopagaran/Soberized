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
pub const MAX_ATTEMPTS: usize = 10;
const MARKER: &str = "soberized-auto-reconnect";
const VALUE: &[u8] = b"gamescope-dialog-v1\n";

pub fn recoverable(code: u16) -> bool {
    matches!(code, 266 | 277 | 278)
}

pub fn retryable_recognition(kind: io::ErrorKind, diagnostic: bool) -> bool {
    !diagnostic && matches!(kind, io::ErrorKind::InvalidData | io::ErrorKind::TimedOut)
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
        if self.attempts >= MAX_ATTEMPTS {
            return Err(io::Error::other(format!(
                "auto-reconnect exhausted {MAX_ATTEMPTS} attempts; input stopped"
            )));
        }
        let delay = match self.attempts {
            0 => 15_000,
            1 => 30_000,
            _ => 60_000,
        };
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
        for name in ["frame.png", "ocr.tsv", "adaptive.tsv", "preference"] {
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
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
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
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Gamescope did not produce a bounded screenshot; no click sent",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let ocr_deadline = Instant::now() + Duration::from_millis(1800);
    let normal = ocr(&scratch.0, &frame, "ocr", false, ocr_deadline)?;
    match detect_button(&normal, dimensions.0, dimensions.1, code) {
        Ok(button) => Ok(button),
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            // A white Reconnect button on a dark dialog can disappear in the
            // default global threshold. Read the SAME frame adaptively too;
            // preserve the confidence, error-code, and uniqueness checks.
            let adaptive = ocr(&scratch.0, &frame, "adaptive", true, ocr_deadline)?;
            detect_readings(&[&normal, &adaptive], dimensions.0, dimensions.1, code)
        }
        Err(error) => Err(error),
    }
}

fn ocr(
    directory: &Path,
    frame: &Path,
    name: &str,
    adaptive: bool,
    deadline: Instant,
) -> io::Result<String> {
    let output = directory.join(name);
    let mut command = Command::new("/usr/bin/tesseract");
    command
        .arg(frame)
        .arg(&output)
        .args(["-l", "eng", "--psm", "11"]);
    if adaptive {
        command.args(["-c", "thresholding_method=1"]);
    }
    command.arg("tsv");
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::TimedOut, "OCR budget expired; no click sent")
        })?;
    run_bounded(&mut command, remaining)?;
    let mut file = File::open(directory.join(format!("{name}.tsv")))?;
    if file.metadata()?.len() > 512 * 1024 {
        return Err(io::Error::other("OCR output exceeded the limit"));
    }
    let mut tsv = String::new();
    file.read_to_string(&mut tsv)?;
    Ok(tsv)
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
    detect_readings(&[tsv], width, height, code)
}

fn merge_label(labels: &mut Vec<Button>, incoming: Button) {
    // Only coalesce readings of the same label BETWEEN threshold passes.
    if !labels.iter().any(|known| {
        known.x.abs_diff(incoming.x) <= 4
            && known.y.abs_diff(incoming.y) <= 4
            && known.width.abs_diff(incoming.width) <= 8
            && known.height.abs_diff(incoming.height) <= 4
    }) {
        labels.push(incoming);
    }
}

fn detect_readings(readings: &[&str], width: u32, height: u32, code: u16) -> io::Result<Button> {
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
    let mut matching_code = false;
    for tsv in readings {
        let mut text = String::new();
        let mut pass_reconnect = Vec::new();
        let mut pass_leave = Vec::new();
        let mut pass_headings = Vec::new();
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
                "reconnect" => pass_reconnect.push(button),
                "leave" => pass_leave.push(button),
                _ => pass_headings.push(button),
            }
        }
        if pass_reconnect.len() > 1 || pass_leave.len() > 1 || pass_headings.len() > 1 {
            return Err(invalid());
        }
        for label in pass_reconnect {
            merge_label(&mut reconnect, label);
        }
        for label in pass_leave {
            merge_label(&mut leave, label);
        }
        for label in pass_headings {
            merge_label(&mut headings, label);
        }
        matching_code |= text.contains(&format!("errorcode{code}"));
        for other in [266, 277, 278] {
            if other != code && text.contains(&format!("errorcode{other}")) {
                return Err(invalid());
            }
        }
    }
    if reconnect.len() != 1 || leave.len() != 1 || headings.len() != 1 || !matching_code {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "disconnect dialog not verified (heading={}, Leave={}, Reconnect={}, matching code={matching_code}); no click sent",
                headings.len(),
                leave.len(),
                reconnect.len()
            ),
        ));
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
        for attempt in 4..=MAX_ATTEMPTS {
            assert_eq!(budget.next_delay().unwrap(), 60_000);
            assert_eq!(budget.attempt(), attempt);
        }
        assert!(budget.next_delay().is_err());
        budget.observe(200_000, true);
        budget.observe(320_000, true);
        assert_eq!(budget.next_delay().unwrap(), 15_000);
        for code in [267, 285, 268, 0, 999] {
            assert!(!recoverable(code));
        }
    }

    #[test]
    fn recognition_misses_consume_all_ten_attempts_without_allowing_a_click() {
        let mut budget = RetryBudget::default();
        let invalid = fixture().replace("Reconnect", "Unreadable");
        for attempt in 1..=MAX_ATTEMPTS {
            budget.next_delay().unwrap();
            let error = detect_button(&invalid, 800, 600, 277).unwrap_err();
            assert!(retryable_recognition(error.kind(), false));
            assert!(!retryable_recognition(error.kind(), true));
            assert_eq!(budget.attempt(), attempt);
        }
        assert!(
            budget
                .next_delay()
                .unwrap_err()
                .to_string()
                .contains("10 attempts")
        );
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::NotFound,
            io::ErrorKind::Other,
        ] {
            assert!(!retryable_recognition(kind, false));
        }
        assert!(retryable_recognition(io::ErrorKind::TimedOut, false));
    }

    #[test]
    fn complementary_thresholds_recognize_a_white_button_on_a_dark_dialog() {
        // Sanitized label-only OCR output from the reported 800x600 dialog.
        // Global thresholding read the heading, code, and Leave but missed the
        // Reconnect label; adaptive Otsu read Reconnect at high confidence.
        let header = fixture().lines().next().unwrap().to_owned();
        let normal = format!(
            "{header}\n\
            5\t1\t1\t1\t1\t1\t343\t193\t115\t15\t90.830803\tDisconnected\n\
            5\t1\t1\t1\t1\t1\t343\t302\t37\t15\t82.279396\t(Error\n\
            5\t1\t1\t1\t1\t1\t387\t302\t38\t12\t92.177711\tCode:\n\
            5\t1\t1\t1\t1\t1\t430\t302\t27\t15\t90.481079\t277)\n\
            5\t1\t1\t1\t1\t1\t290\t382\t38\t11\t93.030396\tLeave\n\
            5\t1\t1\t1\t1\t1\t494\t385\t13\t8\t24.901749\tnn;\n"
        );
        let adaptive =
            format!("{header}\n5\t1\t1\t1\t1\t1\t459\t382\t69\t11\t92.552551\tReconnect\n");
        assert!(detect_button(&normal, 800, 600, 277).is_err());
        assert!(detect_button(&adaptive, 800, 600, 277).is_err());
        let button = detect_readings(&[&normal, &adaptive], 800, 600, 277).unwrap();
        assert_eq!(button.center(), (493, 387));
        assert!(detect_readings(&[&normal, &adaptive], 800, 600, 278).is_err());
        let low_confidence = adaptive.replace("92.552551", "30");
        assert!(detect_readings(&[&normal, &low_confidence], 800, 600, 277).is_err());
    }

    #[test]
    fn threshold_passes_coalesce_only_agreeing_labels_and_reject_conflicts() {
        let normal = fixture();
        assert!(detect_readings(&[&normal, &normal], 800, 600, 277).is_ok());
        let moved = normal.replace("\t440\t340\t", "\t480\t340\t");
        assert!(detect_readings(&[&normal, &moved], 800, 600, 277).is_err());
        let other_code = normal.replace("277)", "278)");
        assert!(detect_readings(&[&normal, &other_code], 800, 600, 277).is_err());
        let duplicate = format!("{normal}5\t1\t1\t1\t1\t1\t440\t340\t100\t20\t95\tReconnect\n");
        assert!(detect_readings(&[&normal, &duplicate], 800, 600, 277).is_err());
    }
}
