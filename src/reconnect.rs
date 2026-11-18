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
        for name in [
            "frame.png",
            "ocr.tsv",
            "button.png",
            "button.tsv",
            "preference",
        ] {
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
    let normal = ocr(&scratch.0, &frame, "ocr", 11, ocr_deadline)?;
    match detect_button(&normal, dimensions.0, dimensions.1, code) {
        Ok(button) => Ok(button),
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            let reading = Reading::parse(&normal, dimensions.0, dimensions.1)?;
            let dialog = reading.dialog(code)?;
            // Never use a crop to bypass conflicting or misplaced labels.
            if !reading.labels("reconnect").is_empty() {
                return Err(error);
            }
            let crop = dialog.crop()?;
            let prepared = scratch.0.join("button.png");
            crate::reconnect_image::prepare(&frame, &prepared, dimensions, crop)?;
            let text = ocr(&scratch.0, &prepared, "button", 7, ocr_deadline)?;
            dialog.cropped_button(&text, crop)
        }
        Err(error) => Err(error),
    }
}

fn ocr(
    directory: &Path,
    frame: &Path,
    name: &str,
    mode: u8,
    deadline: Instant,
) -> io::Result<String> {
    let output = directory.join(name);
    let mut command = Command::new("/usr/bin/tesseract");
    command
        .arg(frame)
        .arg(&output)
        .args(["-l", "eng", "--psm"])
        .arg(mode.to_string());
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

fn invalid_dialog(message: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{message}; no click sent"),
    )
}

#[derive(Debug)]
struct Word {
    text: String,
    bounds: Button,
}

struct Reading {
    words: Vec<Word>,
    dimensions: (u32, u32),
}

impl Reading {
    fn parse(tsv: &str, width: u32, height: u32) -> io::Result<Self> {
        if width == 0 || height == 0 || width > 4096 || height > 2160 {
            return Err(invalid_dialog("invalid OCR dimensions"));
        }
        let mut words = Vec::new();
        let mut text_length = 0;
        for line in tsv.lines().skip(1) {
            if line.len() > 2048 {
                return Err(invalid_dialog("OCR line exceeded its limit"));
            }
            let fields: Vec<_> = line.splitn(12, '\t').collect();
            if fields.len() != 12 || fields[0] != "5" {
                continue;
            }
            let confidence: f32 = fields[10]
                .parse()
                .map_err(|_| invalid_dialog("invalid OCR confidence"))?;
            if !confidence.is_finite() || confidence < 75.0 {
                continue;
            }
            let text: String = fields[11]
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .flat_map(char::to_lowercase)
                .collect();
            if text.is_empty() {
                continue;
            }
            text_length += text.len();
            if text_length > 64 * 1024 {
                return Err(invalid_dialog("OCR text exceeded its limit"));
            }
            let n: Vec<u32> = fields[6..10]
                .iter()
                .map(|s| {
                    s.parse()
                        .map_err(|_| invalid_dialog("invalid OCR coordinates"))
                })
                .collect::<io::Result<_>>()?;
            let bounds = Button {
                x: n[0],
                y: n[1],
                width: n[2],
                height: n[3],
            };
            if bounds.width == 0
                || bounds.height == 0
                || bounds.x.saturating_add(bounds.width) > width
                || bounds.y.saturating_add(bounds.height) > height
            {
                return Err(invalid_dialog("OCR text outside the image"));
            }
            words.push(Word { text, bounds });
        }
        Ok(Self {
            words,
            dimensions: (width, height),
        })
    }

    fn labels(&self, text: &str) -> Vec<&Button> {
        self.words
            .iter()
            .filter(|w| w.text == text)
            .map(|w| &w.bounds)
            .collect()
    }

    fn unique(&self, text: &str) -> io::Result<Button> {
        let labels = self.labels(text);
        if labels.len() != 1 {
            return Err(invalid_dialog(&format!(
                "dialog {text} label is not unique (found {})",
                labels.len()
            )));
        }
        Ok(labels[0].clone())
    }

    fn dialog(&self, expected_code: u16) -> io::Result<Dialog> {
        if !recoverable(expected_code) {
            return Err(invalid_dialog("disconnect reason is not eligible"));
        }
        let heading = self.unique("disconnected")?;
        let leave = self.unique("leave")?;
        let (width, height) = self.dimensions;
        if heading.y + heading.height >= leave.y
            || leave.y <= height / 4
            || leave.y >= height * 9 / 10
            || leave.center().0 >= heading.center().0
        {
            return Err(invalid_dialog("unexpected disconnect dialog layout"));
        }
        let mut codes = Vec::new();
        for start in 0..self.words.len() {
            for count in 1..=3 {
                let Some(parts) = self.words.get(start..start + count) else {
                    continue;
                };
                let combined: String = parts.iter().map(|w| w.text.as_str()).collect();
                let Some(code) = combined
                    .strip_prefix("errorcode")
                    .and_then(|s| s.parse::<u16>().ok())
                else {
                    continue;
                };
                let top = parts.iter().map(|w| w.bounds.y).min().unwrap();
                let bottom = parts
                    .iter()
                    .map(|w| w.bounds.y + w.bounds.height)
                    .max()
                    .unwrap();
                let left = parts.iter().map(|w| w.bounds.x).min().unwrap();
                let right = parts
                    .iter()
                    .map(|w| w.bounds.x + w.bounds.width)
                    .max()
                    .unwrap();
                let max_height = parts.iter().map(|w| w.bounds.height).max().unwrap();
                let aligned = parts.windows(2).all(|pair| {
                    pair[0].bounds.y.abs_diff(pair[1].bounds.y) <= max_height / 2
                        && pair[1].bounds.x >= pair[0].bounds.x + pair[0].bounds.width
                        && pair[1]
                            .bounds
                            .x
                            .saturating_sub(pair[0].bounds.x + pair[0].bounds.width)
                            <= max_height * 4
                });
                if aligned
                    && top > heading.y + heading.height
                    && bottom < leave.y
                    && (left + (right - left) / 2).abs_diff(heading.center().0) <= width / 8
                {
                    codes.push(code);
                }
            }
        }
        if codes.as_slice() != [expected_code] {
            return Err(invalid_dialog(
                "matching error code was not uniquely located inside the dialog",
            ));
        }
        Ok(Dialog {
            heading,
            leave,
            dimensions: self.dimensions,
        })
    }
}

struct Dialog {
    heading: Button,
    leave: Button,
    dimensions: (u32, u32),
}

impl Dialog {
    fn validate(&self, target: &Button) -> io::Result<()> {
        if target.x <= self.leave.x + self.leave.width
            || target.y.abs_diff(self.leave.y) > target.height.max(self.leave.height)
            || target.y <= self.heading.y + self.heading.height
        {
            return Err(invalid_dialog("unexpected Reconnect label position"));
        }
        Ok(())
    }

    fn crop(&self) -> io::Result<crate::reconnect_image::Crop> {
        let center = self.heading.center().0;
        let gap = center.saturating_sub(self.leave.center().0);
        if gap < self.leave.width || gap > self.dimensions.0 / 3 {
            return Err(invalid_dialog("cannot isolate the Reconnect button row"));
        }
        let width = gap * 2;
        let padding = self.leave.height.max(8);
        let y = self
            .leave
            .y
            .checked_sub(padding)
            .ok_or_else(|| invalid_dialog("invalid button row"))?;
        let height = self.leave.height + padding * 2;
        if center + width > self.dimensions.0 || y + height > self.dimensions.1 {
            return Err(invalid_dialog("button row would exceed image bounds"));
        }
        // Predict only a reading region from the centered, two-button layout.
        // No click is allowed unless this region actually reads "Reconnect".
        let scale = 30u32
            .div_ceil(self.leave.height)
            .clamp(1, 3)
            .min(4096 / width);
        Ok(crate::reconnect_image::Crop {
            x: center,
            y,
            width,
            height,
            scale,
        })
    }

    fn cropped_button(&self, tsv: &str, crop: crate::reconnect_image::Crop) -> io::Result<Button> {
        let (width, height) = crop.dimensions();
        let reading = Reading::parse(tsv, width, height)?;
        if reading.words.len() != 1 || reading.words[0].text != "reconnect" {
            return Err(invalid_dialog(
                "isolated button did not uniquely read Reconnect",
            ));
        }
        let word = &reading.words[0].bounds;
        let (x, y, width, height) = crop.map_bounds(word.x, word.y, word.width, word.height)?;
        let button = Button {
            x,
            y,
            width,
            height,
        };
        self.validate(&button)?;
        Ok(button)
    }
}

pub fn detect_button(tsv: &str, width: u32, height: u32, code: u16) -> io::Result<Button> {
    let reading = Reading::parse(tsv, width, height)?;
    let dialog = reading.dialog(code)?;
    let button = reading.unique("reconnect")?;
    dialog.validate(&button)?;
    Ok(button)
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

    fn reported_context() -> String {
        let header = fixture().lines().next().unwrap().to_owned();
        format!(
            "{header}\n\
            5\t1\t1\t1\t1\t1\t343\t193\t115\t15\t90.830803\tDisconnected\n\
            5\t1\t1\t1\t1\t1\t343\t302\t37\t15\t82.279396\t(Error\n\
            5\t1\t1\t1\t1\t1\t387\t302\t38\t12\t92.177711\tCode:\n\
            5\t1\t1\t1\t1\t1\t430\t302\t27\t15\t90.481079\t277)\n\
            5\t1\t1\t1\t1\t1\t290\t382\t38\t11\t93.030396\tLeave\n\
            5\t1\t1\t1\t1\t1\t494\t385\t13\t8\t24.901749\tnn;\n"
        )
    }

    #[test]
    fn button_crop_requires_valid_context_and_a_real_high_confidence_label() {
        let normal = reported_context();
        assert!(detect_button(&normal, 800, 600, 277).is_err());
        let reading = Reading::parse(&normal, 800, 600).unwrap();
        let dialog = reading.dialog(277).unwrap();
        let crop = dialog.crop().unwrap();
        assert_eq!(
            crop,
            crate::reconnect_image::Crop {
                x: 400,
                y: 371,
                width: 182,
                height: 33,
                scale: 3
            }
        );
        let header = fixture().lines().next().unwrap().to_owned();
        let isolated = format!("{header}\n5\t1\t1\t1\t1\t1\t177\t33\t207\t33\t92.55\tReconnect\n");
        let button = dialog.cropped_button(&isolated, crop).unwrap();
        assert_eq!(button.center(), (493, 387));
        assert!(
            dialog
                .cropped_button(&isolated.replace("92.55", "30"), crop)
                .is_err()
        );
        assert!(
            dialog
                .cropped_button(&isolated.replace("Reconnect", "Leave"), crop)
                .is_err()
        );
        assert!(
            dialog
                .cropped_button(&isolated.replace("\t177\t33\t", "\t0\t33\t"), crop)
                .is_err()
        );
        assert!(reading.dialog(278).is_err());
    }

    #[test]
    fn context_cannot_borrow_error_codes_from_elsewhere_or_resolve_ambiguity_with_a_crop() {
        let normal = reported_context();
        let unrelated = normal.replace("\t302\t", "\t50\t");
        assert!(
            Reading::parse(&unrelated, 800, 600)
                .unwrap()
                .dialog(277)
                .is_err()
        );
        let duplicate = format!("{normal}5\t1\t1\t1\t1\t1\t350\t190\t100\t20\t95\tDisconnected\n");
        assert!(
            Reading::parse(&duplicate, 800, 600)
                .unwrap()
                .dialog(277)
                .is_err()
        );
        let header = fixture().lines().next().unwrap().to_owned();
        let dialog = Reading::parse(&normal, 800, 600)
            .unwrap()
            .dialog(277)
            .unwrap();
        let crop = dialog.crop().unwrap();
        let multiple = format!(
            "{header}\n\
            5\t1\t1\t1\t1\t1\t177\t33\t207\t33\t95\tReconnect\n\
            5\t1\t1\t1\t1\t1\t10\t10\t100\t20\t95\tReconnect\n"
        );
        assert!(dialog.cropped_button(&multiple, crop).is_err());
        let extra_text =
            multiple.replace("10\t10\t100\t20\t95\tReconnect", "10\t10\t100\t20\t95\tBuy");
        assert!(dialog.cropped_button(&extra_text, crop).is_err());
    }

    #[test]
    fn changed_or_out_of_bounds_dialog_geometry_is_rejected() {
        let normal = reported_context();
        let leave_on_right = normal.replace("\t290\t382\t", "\t500\t382\t");
        assert!(
            Reading::parse(&leave_on_right, 800, 600)
                .unwrap()
                .dialog(277)
                .is_err()
        );
        let low_leave = normal.replace("\t93.030396\tLeave", "\t20\tLeave");
        assert!(
            Reading::parse(&low_leave, 800, 600)
                .unwrap()
                .dialog(277)
                .is_err()
        );
        let dialog = Reading::parse(&normal, 800, 600)
            .unwrap()
            .dialog(277)
            .unwrap();
        assert!(
            dialog
                .validate(&Button {
                    x: 290,
                    y: 382,
                    width: 69,
                    height: 11
                })
                .is_err()
        );
        assert!(
            dialog
                .validate(&Button {
                    x: 459,
                    y: 430,
                    width: 69,
                    height: 11
                })
                .is_err()
        );
    }
}
