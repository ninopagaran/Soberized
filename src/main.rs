use soberized::ipc::Client;
use soberized::launch::{self as launch_config, Mode as LaunchMode};
use soberized::protocol::{Command as Operation, ReplyCode, State};
use soberized::units::{self, SOCKET_UNIT};
use soberized::{
    APP_ID, HELPER_PATH, HELPER_UNIT, SOCKET_PATH, USER_UNIT, helper, notify, roblox_log, sober,
};
use std::io;
use std::os::unix::fs::FileTypeExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

mod gamescope_input;
mod reconnect;
mod reconnect_image;

fn main() {
    if let Err(error) = run() {
        eprintln!("soberized: {error}");
        std::process::exit(1);
    }
}

fn run() -> io::Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.is_empty() || arguments == ["--help"] || arguments == ["help"] {
        println!(
            "Soberized {}\n\nUsage: soberized <command>\n\n  doctor [--sandbox-test]  Check installation and optional syscall self-test\n  launch [--gamescope]      Prepare a gamepad, then launch Sober (optional nested display)\n    --capture-mouse        With --gamescope: opt into camera mouse capture\n  start [--auto-reconnect] Arm input; optionally recover recognized disconnects\n    --auto-reconnect       With Gamescope: recognize and click Reconnect on eligible errors\n  stop                     Stop input and remove the virtual controller\n  toggle                   Toggle the session controller (for a Sway shortcut)\n  status                   Show the controller and next scheduled pulse\n  test-input               Queue one brief movement pulse in 10 seconds\n  test-ui-navigation       Test one gamepad Select press without confirming a button\n  check-reconnect          Check Gamescope input and dialog OCR without sending input\n  test-reconnect           Try one recognized Reconnect click after a disconnect\n\nPulses can move your avatar. Park in a safe location before starting.\nEnable gamepad access in Sober Settings before use. Stop before playing other games.\nBackground anti-idle compatibility is experimental; see README.md.",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }
    if arguments == ["--version"] {
        println!("soberized {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    // SAFETY: geteuid has no preconditions. Administration belongs to installation.
    if unsafe { libc::geteuid() } == 0 {
        return Err(io::Error::other(
            "run Soberized as your desktop user, without sudo",
        ));
    }
    if arguments == ["doctor"] || arguments == ["doctor", "--sandbox-test"] {
        return doctor(arguments.len() == 2);
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "launch")
    {
        let (options, auto_reconnect) = reconnect_option(&arguments[1..])?;
        return launch(LaunchMode::parse(&options)?, auto_reconnect);
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "start")
    {
        let (options, auto_reconnect) = reconnect_option(&arguments[1..])?;
        if !options.is_empty() {
            return Err(io::Error::other(
                "usage: soberized start [--auto-reconnect]",
            ));
        }
        return start(auto_reconnect);
    }
    if arguments.len() != 1 {
        return Err(io::Error::other("unknown arguments; use soberized --help"));
    }
    match arguments[0].as_str() {
        "stop" => stop(),
        "toggle" if unit_active(true, USER_UNIT) => stop(),
        "toggle" => start(false),
        "status" => status(),
        "test-input" => test_input(),
        "test-ui-navigation" => test_ui_navigation(),
        "test-reconnect" => test_reconnect(),
        "check-reconnect" => check_reconnect(),
        "session" => session(), // Invoked by the root-owned systemd user unit.
        _ => Err(io::Error::other("unknown command; use soberized --help")),
    }
}

fn reconnect_option(options: &[String]) -> io::Result<(Vec<String>, bool)> {
    let mut filtered = Vec::new();
    let mut enabled = false;
    for option in options {
        if option == "--auto-reconnect" {
            if enabled {
                return Err(io::Error::other("duplicate --auto-reconnect option"));
            }
            enabled = true;
        } else {
            filtered.push(option.clone());
        }
    }
    Ok((filtered, enabled))
}

fn unit_active(user: bool, unit: &str) -> bool {
    let mut command = Command::new("/usr/bin/systemctl");
    if user {
        command.arg("--user");
    }
    command
        .args(["is-active", "--quiet", unit])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn control_service(action: &str) -> io::Result<()> {
    let status = Command::new("/usr/bin/systemctl")
        .args(["--user", action, USER_UNIT])
        .stdin(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(io::Error::other(
            "could not control session service; inspect journalctl --user -u soberized-session.service",
        ));
    }
    Ok(())
}

fn installed() -> io::Result<()> {
    let helper = units::query(false, HELPER_UNIT)?;
    let socket = units::query(false, SOCKET_UNIT)?;
    let metadata = match std::fs::symlink_metadata(SOCKET_PATH) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if let Some(problem) = units::backend_problem(&helper, &socket, metadata.is_some()) {
        return Err(io::Error::other(problem));
    }
    let metadata =
        metadata.ok_or_else(|| io::Error::other("helper socket disappeared; retry the command"))?;
    if !metadata.file_type().is_socket() {
        return Err(io::Error::other("control path is not a socket"));
    }
    Ok(())
}

fn start(auto_reconnect: bool) -> io::Result<()> {
    installed()?;
    let target = sober::instance()?
        .ok_or_else(|| io::Error::other("Sober is not running; use soberized launch"))?;
    if auto_reconnect {
        reconnect::dependencies()?;
        gamescope_input::Endpoint::locate(&target)?;
    }
    if unit_active(true, USER_UNIT) && reconnect::enabled()? != auto_reconnect {
        return Err(io::Error::other(
            "run soberized stop before changing auto-reconnect mode",
        ));
    }
    reconnect::set_enabled(auto_reconnect)?;
    let result = control_service("start").and_then(|()| wait_for_controller(false));
    if let Err(error) = result {
        let _ = control_service("stop");
        let _ = reconnect::set_enabled(false);
        return Err(error);
    }
    println!(
        "Session controller started. Pulses briefly move your avatar; use a safe location. Check soberized status to confirm input is armed."
    );
    Ok(())
}

fn stop() -> io::Result<()> {
    let preference = reconnect::set_enabled(false);
    let service = control_service("stop");
    if unit_active(false, HELPER_UNIT) {
        // The worker usually closes its lease first. Tolerate that cleanup race.
        if let Ok(client) = Client::connect(Path::new(SOCKET_PATH)) {
            let _ = client.command(Operation::Stop);
        }
    }
    service?;
    preference?;
    println!("Stopped.");
    Ok(())
}

fn status() -> io::Result<()> {
    println!("Soberized {}", env!("CARGO_PKG_VERSION"));
    installed()?;
    let session = units::query(true, USER_UNIT)?;
    let controller = unit_active(true, USER_UNIT);
    if controller && reconnect::enabled()? {
        println!(
            "Auto-reconnect: enabled (Gamescope dialog recognition; up to {} attempts).",
            reconnect::MAX_ATTEMPTS
        );
    } else if controller {
        println!("Auto-reconnect: disabled; use start --auto-reconnect to enable.");
    }
    let response = if unit_active(false, HELPER_UNIT) {
        Some(Client::connect(Path::new(SOCKET_PATH))?.command(Operation::Status)?)
    } else {
        None
    };
    let diagnostic = !controller
        && response
            .is_some_and(|response| matches!(response.state, State::Prepared | State::Selecting));
    if session.failed() && !diagnostic {
        if session.status.starts_with("Roblox disconnected:") {
            return Err(io::Error::other(session.status));
        }
        return Err(io::Error::other(format!(
            "session controller failed ({}): {}; inspect journalctl --user -u {USER_UNIT} -b",
            session.result, session.status
        )));
    }
    let Some(response) = response else {
        println!(
            "{}",
            if controller {
                if session.status.is_empty() {
                    "Session controller is starting; check its journal if this persists."
                } else {
                    &session.status
                }
            } else {
                "Stopped."
            }
        );
        return Ok(());
    };
    match response.state {
        State::Ready => println!("Waiting for a session lease; no pulses scheduled."),
        State::Prepared => println!(
            "{}",
            if !controller {
                "Neutral diagnostic controller present; movement is disabled."
            } else if session.status.starts_with("Waiting for ")
                || session.status.starts_with("Reconnecting:")
            {
                &session.status
            } else {
                "Virtual controller prepared; movement disabled until a Roblox game join."
            }
        ),
        State::Armed => println!(
            "Armed (brief movement pulses). Next pulse in {}s; {} pulses sent.\nRoblox: monitoring client disconnects; server connection is unverified.",
            response.next_seconds, response.pulses
        ),
        State::Pulsing => println!(
            "Sending a bounded controller pulse; {} pulses sent.",
            response.pulses
        ),
        State::Selecting => println!(
            "UI selection diagnostic queued or active; Select starts in {}s. Movement is disabled.",
            response.next_seconds
        ),
    }
    Ok(())
}

fn test_input() -> io::Result<()> {
    installed()?;
    if !unit_active(true, USER_UNIT) || !unit_active(false, HELPER_UNIT) {
        return Err(io::Error::other(
            "start an active Soberized session before testing input",
        ));
    }
    let response = Client::connect(Path::new(SOCKET_PATH))?.command(Operation::TestInput)?;
    match response.code {
        ReplyCode::Ok => println!(
            "One 250 ms movement pulse is queued for 10 seconds from now. Place your avatar safely and watch for a brief step, then rest. Check the helper journal for deflect and neutral records."
        ),
        ReplyCode::Busy => {
            return Err(io::Error::other(
                "a test is already queued, a pulse is active, or the one-minute test cooldown has not elapsed",
            ));
        }
        ReplyCode::NotOwner => {
            return Err(io::Error::other(
                "movement is not armed; join a Roblox experience and check soberized status before testing input",
            ));
        }
    }
    Ok(())
}

fn launch(mode: LaunchMode, auto_reconnect: bool) -> io::Result<()> {
    // Check optional dependencies before activating or replacing any lease.
    mode.check_requirements()?;
    if auto_reconnect {
        if !mode.uses_gamescope() {
            return Err(io::Error::other("--auto-reconnect requires --gamescope"));
        }
        reconnect::dependencies()?;
    }
    installed()?;
    if sober::instance()?.is_some() {
        return Err(io::Error::other(
            "Sober is already running; close it before launching with session-only gamepad permissions, or use start for an already prepared instance",
        ));
    }
    // The worker owns a neutral-only lease before Flatpak starts and
    // Sober initializes its controller handling. It arms only after a game join.
    reconnect::set_enabled(auto_reconnect)?;
    let result = control_service("restart").and_then(|()| launch_prepared(mode));
    if result.is_err() {
        let _ = control_service("stop");
        let _ = reconnect::set_enabled(false);
    }
    result
}

fn test_ui_navigation() -> io::Result<()> {
    installed()?;
    if unit_active(true, USER_UNIT) {
        return Err(io::Error::other(
            "run soberized stop before the selection-only diagnostic; it never replaces an active input lease",
        ));
    }
    let target = sober::instance()?.ok_or_else(|| {
        io::Error::other("Sober is not running; use soberized launch first, then soberized stop")
    })?;
    let mut lease = SessionLease::prepare()?;
    let mut accessible = false;
    for _ in 0..50 {
        lease.check()?;
        if gamepad_accessible()? {
            accessible = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if !accessible {
        return Err(io::Error::other(
            "the diagnostic controller has no readable event node; inspect udev seat ACLs",
        ));
    }
    if lease.client.command(Operation::UiSelect)?.code != ReplyCode::Ok {
        return Err(io::Error::other(
            "could not queue the UI selection diagnostic",
        ));
    }
    println!(
        "In 10 seconds, the virtual gamepad will press Select once for 100 ms, then release all controls. Focus Gamescope and watch for a UI highlight. No direction, confirm, Enter, or mouse input is sent."
    );
    println!(
        "The diagnostic keeps the neutral controller present for another 10 seconds, then removes it. Ctrl+C or soberized stop cancels the test."
    );
    let deadline = lease.check()?
        + soberized::schedule::TEST_DELAY_MS
        + soberized::schedule::UI_SELECT_MS
        + 10_000;
    let mut released = false;
    loop {
        let now = lease.check()?;
        if sober::instance()?.as_ref() != Some(&target) {
            return Err(io::Error::other(
                "Sober instance ended or changed; diagnostic cancelled",
            ));
        }
        let response = lease.client.command(Operation::Status)?;
        match response.state {
            State::Selecting => {}
            State::Prepared => {
                if !released {
                    println!(
                        "Select was sent and released. Check whether a button is highlighted."
                    );
                    released = true;
                }
            }
            _ => return Err(io::Error::other("diagnostic lease changed; test stopped")),
        }
        if now >= deadline {
            println!("Selection-only diagnostic finished; controller removed on exit.");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn wait_for_controller(prepared_only: bool) -> io::Result<()> {
    for _ in 0..50 {
        if !unit_active(true, USER_UNIT) {
            return Err(io::Error::other(
                "session controller stopped during preparation; inspect journalctl --user -u soberized-session.service",
            ));
        }
        let response = Client::connect(Path::new(SOCKET_PATH))?.command(Operation::Status)?;
        match response.state {
            State::Prepared => return Ok(()),
            State::Armed | State::Pulsing if !prepared_only => return Ok(()),
            State::Armed | State::Pulsing => {
                return Err(io::Error::other(
                    "Sober started during controller preparation; close it and retry soberized launch",
                ));
            }
            State::Ready => std::thread::sleep(Duration::from_millis(100)),
            State::Selecting => {
                return Err(io::Error::other(
                    "a UI selection diagnostic is active; finish it before starting movement",
                ));
            }
        }
    }
    Err(io::Error::other(
        "virtual controller preparation timed out; inspect the helper and session journals",
    ))
}

fn launch_prepared(mode: LaunchMode) -> io::Result<()> {
    wait_for_controller(true)?;
    // uinput creation and udev's node/seat ACL setup are asynchronous. Do not
    // launch a client until the desktop user can read the matching event node.
    let mut accessible = false;
    for _ in 0..50 {
        if gamepad_accessible()? {
            accessible = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if !accessible {
        return Err(io::Error::other(
            "the prepared gamepad has no readable event node; inspect udev seat ACLs before retrying",
        ));
    }
    if sober::instance()?.is_some() {
        return Err(io::Error::other(
            "Sober started during controller preparation; close it and retry soberized launch",
        ));
    }
    let mut child = mode
        .command()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()?;
    let result = (|| {
        for _ in 0..30 {
            if let Some(exit) = child.try_wait()? {
                return Err(io::Error::other(format!(
                    "Sober launcher exited during startup ({exit})"
                )));
            }
            if sober::instance_when_ready()?.is_some() {
                println!(
                    "Sober launched with the virtual controller already present. Movement stays disabled until a Roblox game join; check soberized status."
                );
                if mode.uses_gamescope() {
                    println!(
                        "Using Gamescope's nested Wayland display. Background input and idle prevention require validation."
                    );
                }
                if mode.captures_mouse() {
                    println!(
                        "Mouse capture enabled for Gamescope. Use your Sway workspace shortcut to leave the window; check pointer release before resuming background use."
                    );
                }
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        Err(io::Error::other(
            "Sober startup timed out; the controller was stopped. Close Sober and retry soberized launch",
        ))
    })();
    if result.is_err() && mode.uses_gamescope() {
        // A failed compositor launch should not leave its empty window running.
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn gamepad_accessible() -> io::Result<bool> {
    use std::ffi::CString;
    for entry in std::fs::read_dir("/sys/class/input")? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(number) = name.to_str().and_then(|name| name.strip_prefix("event")) else {
            continue;
        };
        if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        if std::fs::read_to_string(entry.path().join("device/name"))
            .is_ok_and(|name| name.trim_end() == soberized::device::NAME)
        {
            let node = format!("/dev/input/event{number}");
            if !std::fs::symlink_metadata(&node)
                .is_ok_and(|metadata| metadata.file_type().is_char_device())
            {
                continue;
            }
            let node = CString::new(node).expect("fixed prefix and decimal event number");
            // SAFETY: a valid NUL-terminated path. This checks access without
            // opening or reading any input device, including physical devices.
            if unsafe { libc::access(node.as_ptr(), libc::R_OK) } == 0 {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

struct SessionLease {
    client: Client,
    last_check: u64,
    next_renewal: u64,
}

impl SessionLease {
    fn pause(&self) -> io::Result<()> {
        let response = self.client.command(Operation::Pause)?;
        if response.code != ReplyCode::Ok || response.state != State::Prepared {
            return Err(io::Error::other("could not pause movement for recovery"));
        }
        Ok(())
    }
    fn prepare() -> io::Result<Self> {
        let client = Client::connect(Path::new(SOCKET_PATH))?;
        let response = client.command(Operation::Prepare)?;
        if response.code != ReplyCode::Ok || response.state != State::Prepared {
            return Err(io::Error::other(
                "another controller already owns the input lease",
            ));
        }
        let now = helper::boot_ms()?;
        Ok(Self {
            client,
            last_check: now,
            next_renewal: now + 10_000,
        })
    }

    fn check(&mut self) -> io::Result<u64> {
        let now = helper::boot_ms()?;
        if now < self.last_check || now.saturating_sub(self.last_check) > 5_000 {
            return Err(io::Error::other(
                "Soberized stopped after suspend, a clock regression, or a scheduling gap; restart explicitly to resume",
            ));
        }
        self.last_check = now;
        if now >= self.next_renewal {
            let response = self.client.command(Operation::Renew)?;
            if response.code != ReplyCode::Ok || response.state == State::Ready {
                return Err(io::Error::other(
                    "helper no longer holds this session lease",
                ));
            }
            self.next_renewal = now + 10_000;
        }
        Ok(now)
    }
}

fn recovery_observe(
    lease: &mut SessionLease,
    target: &sober::Instance,
    monitor: &mut roblox_log::Monitor,
    baseline: (u64, u64),
    test: bool,
) -> io::Result<bool> {
    lease.check()?;
    if !test && !reconnect::enabled()? {
        return Err(io::Error::other("auto-reconnect was disabled"));
    }
    if sober::instance()?.as_ref() != Some(target) {
        return Err(io::Error::other(
            "Sober instance ended or changed; recovery cancelled",
        ));
    }
    monitor.poll()?;
    Ok(reconnect::observe(
        monitor.latest(),
        monitor.connected(),
        monitor.connection_revision(),
        monitor.generation(),
        baseline,
    )? == reconnect::Observation::Reconnected)
}

fn recognition_failure(error: io::Error, attempt: usize, diagnostic: bool) -> io::Result<String> {
    if !reconnect::retryable_recognition(error.kind(), diagnostic) {
        return Err(error);
    }
    let reason = error.to_string();
    let message = format!(
        "Reconnecting: attempt {attempt}/{} could not verify the dialog; no click sent: {error}",
        reconnect::MAX_ATTEMPTS
    );
    notify::status(&message)?;
    eprintln!("{message}");
    Ok(reason)
}

fn recovery_wait_status(
    attempt: usize,
    limit: usize,
    remaining: u64,
    last_issue: Option<&str>,
) -> String {
    let mut message = format!(
        "Reconnecting: attempt {attempt}/{limit} in {}s; movement paused",
        remaining.div_ceil(1000)
    );
    if let Some(issue) = last_issue {
        message.push_str(&format!(". Last issue: {issue}"));
    }
    message
}

fn recover(
    lease: &mut SessionLease,
    target: &sober::Instance,
    monitor: &mut roblox_log::Monitor,
    retries: &mut reconnect::RetryBudget,
    test: bool,
) -> io::Result<()> {
    let result = (|| {
        lease.pause()?;
        reconnect::dependencies()?;
        let endpoint = gamescope_input::Endpoint::locate(target)?;
        let baseline = (monitor.connection_revision(), monitor.generation());
        let limit = if test { 1 } else { reconnect::MAX_ATTEMPTS };
        let mut last_issue: Option<String> = None;
        'attempts: loop {
            let backoff = retries.next_delay()?;
            let delay = if test { 10_000 } else { backoff };
            let message =
                recovery_wait_status(retries.attempt(), limit, delay, last_issue.as_deref());
            notify::status(&message)?;
            eprintln!("{message}");
            let due = lease.check()? + delay;
            let mut next_status = lease.check()? + 1000;
            while lease.check()? < due {
                if recovery_observe(lease, target, monitor, baseline, test)? {
                    return Ok(());
                }
                let now = lease.check()?;
                if now >= next_status {
                    notify::status(&recovery_wait_status(
                        retries.attempt(),
                        limit,
                        due.saturating_sub(now),
                        last_issue.as_deref(),
                    ))?;
                    next_status = now + 1000;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            if recovery_observe(lease, target, monitor, baseline, test)? {
                return Ok(());
            }
            let code = match monitor.latest() {
                Some(roblox_log::Event::Disconnected { code, .. }) => *code,
                _ => {
                    return Err(io::Error::other(
                        "a game join is already in progress; no click sent",
                    ));
                }
            };
            endpoint.verify(target)?;
            let mut pointer = endpoint.pointer()?;
            pointer.park()?;
            if recovery_observe(lease, target, monitor, baseline, test)? {
                return Ok(());
            }
            let mut captures = Vec::with_capacity(2);
            for _ in 0..2 {
                let button = match reconnect::find_button(&endpoint, code) {
                    Ok(button) => button,
                    Err(error) => {
                        last_issue = Some(recognition_failure(error, retries.attempt(), test)?);
                        continue 'attempts;
                    }
                };
                captures.push(button);
                // Lease, log and client identity errors are fatal regardless of
                // their ErrorKind. Only capture/recognition errors may retry.
                if recovery_observe(lease, target, monitor, baseline, test)? {
                    return Ok(());
                }
            }
            let second = captures.pop().unwrap();
            if captures[0] != second {
                last_issue = Some(recognition_failure(
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "disconnect dialog changed between captures; no click sent",
                    ),
                    retries.attempt(),
                    test,
                )?);
                continue;
            }
            if !matches!(monitor.latest(), Some(roblox_log::Event::Disconnected { code: current, .. }) if *current == code)
            {
                return Err(io::Error::other("disconnect state changed; no click sent"));
            }
            endpoint.verify(target)?;
            if recovery_observe(lease, target, monitor, baseline, test)? {
                return Ok(());
            }
            if !matches!(monitor.latest(), Some(roblox_log::Event::Disconnected { code: current, .. }) if *current == code)
            {
                return Err(io::Error::other("disconnect state changed; no click sent"));
            }
            let (x, y) = second.center();
            pointer.click(x, y)?;
            drop(pointer);
            eprintln!(
                "Reconnect click sent to the bound Gamescope instance; attempt={} code={code}",
                retries.attempt()
            );
            notify::status(
                "Reconnecting: click sent; waiting for a fresh client connection marker, movement paused",
            )?;
            let deadline = lease.check()? + reconnect::JOIN_WAIT_MS;
            let mut next_status = lease.check()? + 1000;
            while lease.check()? < deadline {
                if recovery_observe(lease, target, monitor, baseline, test)? {
                    eprintln!("A fresh client connection marker was observed after recovery.");
                    return Ok(());
                }
                let now = lease.check()?;
                if now >= next_status {
                    notify::status(&format!(
                        "Reconnecting: click sent; waiting for a fresh connection marker ({}s); movement paused",
                        deadline.saturating_sub(now).div_ceil(1000)
                    ))?;
                    next_status = now + 1000;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            if test {
                return Err(io::Error::other(
                    "no fresh connection marker within 90 seconds; diagnostic ended",
                ));
            }
            last_issue = Some("Reconnect click had no fresh connection marker within 90s".into());
        }
    })();
    if let Err(error) = &result {
        let _ = notify::status(&format!("Auto-reconnect stopped: {error}. Input disabled."));
    }
    result
}

fn test_reconnect() -> io::Result<()> {
    installed()?;
    reconnect::dependencies()?;
    if unit_active(true, USER_UNIT) {
        return Err(io::Error::other(
            "run soberized stop before testing reconnect",
        ));
    }
    let target = sober::instance()?.ok_or_else(|| io::Error::other("Sober is not running"))?;
    let mut monitor =
        roblox_log::Monitor::attach(roblox_log::default_path()?, sober::started_at(&target)?)?;
    if !matches!(monitor.latest(), Some(roblox_log::Event::Disconnected { code, .. }) if reconnect::recoverable(*code))
    {
        return Err(io::Error::other(
            "no eligible disconnect is currently logged; no click sent",
        ));
    }
    gamescope_input::Endpoint::locate(&target)?;
    let mut lease = SessionLease::prepare()?;
    println!(
        "Reconnect diagnostic: in ten seconds, recognize the matching dialog and send at most one click inside Gamescope. Ctrl+C or soberized stop cancels."
    );
    recover(
        &mut lease,
        &target,
        &mut monitor,
        &mut reconnect::RetryBudget::default(),
        true,
    )?;
    println!(
        "A fresh client connection marker was observed. Anti-idle movement stays disabled until soberized start."
    );
    Ok(())
}

fn check_reconnect() -> io::Result<()> {
    reconnect::dependencies()?;
    let target = sober::instance()?.ok_or_else(|| io::Error::other("Sober is not running"))?;
    let endpoint = gamescope_input::Endpoint::locate(&target)?;
    let _pointer = endpoint.pointer()?;
    println!("Bound Gamescope input handshake passed. No input sent.");
    let monitor =
        roblox_log::Monitor::attach(roblox_log::default_path()?, sober::started_at(&target)?)?;
    if let Some(roblox_log::Event::Disconnected { code, .. }) = monitor.latest() {
        if reconnect::recoverable(*code) {
            let button = reconnect::find_button(&endpoint, *code)?;
            let (x, y) = button.center();
            println!(
                "Matching disconnect dialog recognized (code {code}); Reconnect at {x},{y}. No click sent."
            );
            return Ok(());
        }
    }
    match reconnect::find_button(&endpoint, 277) {
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            println!("Screenshot/OCR completed; no matching disconnect dialog. No input sent.")
        }
        Ok(_) => println!(
            "A dialog is visible without an eligible disconnect log; input remains disabled."
        ),
        Err(error) => return Err(error),
    }
    Ok(())
}

fn session() -> io::Result<()> {
    installed()?;
    eprintln!(
        "Soberized session {}: preparing a neutral controller",
        env!("CARGO_PKG_VERSION")
    );
    let mut lease = SessionLease::prepare()?;
    let auto_reconnect = reconnect::enabled()?;
    let mut retries = reconnect::RetryBudget::default();
    let startup_deadline = lease.last_check + 30_000;
    notify::status("Waiting for Sober startup; virtual controller prepared, movement disabled")?;
    let target = loop {
        let now = lease.check()?;
        if let Some(target) = sober::instance_when_ready()? {
            break target;
        }
        if now >= startup_deadline {
            return Err(io::Error::other(
                "Sober did not start within 30 seconds; the neutral controller was removed",
            ));
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let mut monitor =
        roblox_log::Monitor::attach(roblox_log::default_path()?, sober::started_at(&target)?)?;
    notify::status(
        "Waiting for a Roblox game join; virtual controller prepared, movement disabled",
    )?;
    loop {
        lease.check()?;
        if sober::instance()?.as_ref() != Some(&target) {
            return Ok(());
        }
        monitor.poll()?;
        match monitor.latest() {
            Some(roblox_log::Event::Disconnected { code: 285, .. }) => {
                std::thread::sleep(Duration::from_secs(2))
            }
            Some(event @ roblox_log::Event::Disconnected { .. }) => {
                if auto_reconnect
                    && matches!(event, roblox_log::Event::Disconnected { code, .. } if reconnect::recoverable(*code))
                {
                    recover(&mut lease, &target, &mut monitor, &mut retries, false)?;
                    break;
                }
                let message = event.disconnect_message(None).unwrap();
                notify::status(&message)?;
                return Err(io::Error::other(message));
            }
            Some(roblox_log::Event::Joined { .. }) => break,
            None => std::thread::sleep(Duration::from_secs(2)),
        }
    }
    if lease.client.command(Operation::Arm)?.code != ReplyCode::Ok {
        return Err(io::Error::other(
            "another controller already owns the input lease",
        ));
    }
    notify::status("Movement pulses active; monitoring Roblox disconnects")?;
    eprintln!(
        "Soberized armed for Sober instance {}. Movement pulse: 250 ms at 50% left-stick deflection every 10 minutes; use a safe location and stop before other games.",
        target.id
    );
    let mut transition_since = None;
    loop {
        std::thread::sleep(Duration::from_secs(2));
        let current = sober::instance()?;
        let now = lease.check()?;
        if current.as_ref() != Some(&target) {
            eprintln!("Sober instance ended or changed; Soberized stopped.");
            return Ok(());
        }
        monitor.poll()?;
        retries.observe(now, monitor.connected());
        if matches!(
            monitor.latest(),
            Some(roblox_log::Event::Disconnected { code: 285, .. })
        ) {
            let since = *transition_since.get_or_insert(now);
            // A teleport also closes the old connection. Give the next join
            // time to arrive before treating this as returning to the home UI.
            if now.saturating_sub(since) >= 3_000 {
                notify::status(
                    "Roblox session ended; input stopped. Join an experience and run soberized start.",
                )?;
                return Ok(());
            }
        } else if let Some(event @ roblox_log::Event::Disconnected { .. }) = monitor.latest() {
            if auto_reconnect
                && matches!(event, roblox_log::Event::Disconnected { code, .. } if reconnect::recoverable(*code))
            {
                recover(&mut lease, &target, &mut monitor, &mut retries, false)?;
                if lease.client.command(Operation::Arm)?.code != ReplyCode::Ok {
                    return Err(io::Error::other("could not rearm after reconnect"));
                }
                notify::status(
                    "Movement pulses resumed after a fresh client connection marker; auto-reconnect enabled",
                )?;
                transition_since = None;
                continue;
            }
            let pulses = lease
                .client
                .command(Operation::Status)
                .ok()
                .map(|response| response.pulses);
            let message = event.disconnect_message(pulses).unwrap();
            notify::status(&message)?;
            // Default and ineligible disconnects still drop the owner lease.
            return Err(io::Error::other(message));
        } else if monitor.latest().is_none() {
            notify::status(
                "Sober log disappeared; input stopped. Check the client log and restart Soberized.",
            )?;
            return Err(io::Error::other("Sober log disappeared; input stopped"));
        } else {
            transition_since = None;
        }
    }
}

fn doctor(sandbox_test: bool) -> io::Result<()> {
    println!("Soberized CLI: {}", env!("CARGO_PKG_VERSION"));
    println!(
        "Gamescope (optional nested display): {}",
        if launch_config::gamescope_installed() {
            "installed"
        } else {
            "not installed"
        }
    );
    let mut ready = true;
    let output = Command::new("/usr/bin/flatpak")
        .args(["info", APP_ID])
        .output()?;
    if output.status.success() {
        println!("Sober: installed");
    } else {
        println!("Sober: not installed");
        ready = false;
    }
    match installed() {
        Ok(()) => println!("Helper socket: installed"),
        Err(error) => {
            println!("Helper socket: {error}");
            ready = false;
        }
    }
    match sober::instance() {
        Ok(Some(instance)) => {
            println!("Sober instance: running");
            let observed = (|| -> io::Result<Option<roblox_log::Event>> {
                let monitor = roblox_log::Monitor::attach(
                    roblox_log::default_path()?,
                    sober::started_at(&instance)?,
                )?;
                Ok(monitor.latest().cloned())
            })();
            match observed {
                Ok(Some(roblox_log::Event::Disconnected { code: 285, .. })) => {
                    println!("Roblox log: waiting for the next game join")
                }
                Ok(Some(event @ roblox_log::Event::Disconnected { .. })) => {
                    println!("{}", event.disconnect_message(None).unwrap());
                    ready = false;
                }
                Ok(Some(roblox_log::Event::Joined { timestamp })) => println!(
                    "Roblox log: last observed game join at {timestamp}; connection is unverified"
                ),
                Ok(None) => println!("Roblox log: waiting for a game join"),
                Err(error) => {
                    println!("Roblox log: {error}");
                    ready = false;
                }
            }
        }
        Ok(None) => println!("Sober instance: stopped"),
        Err(error) => {
            println!("Sober instance: {error}");
            ready = false;
        }
    }
    let groups = std::fs::read_to_string("/proc/self/status")?;
    let input_gid = std::fs::read_to_string("/etc/group")?
        .lines()
        .find_map(|line| {
            let columns: Vec<_> = line.split(':').collect();
            (columns.first() == Some(&"input"))
                .then(|| columns.get(2).and_then(|s| s.parse::<u32>().ok()))
                .flatten()
        });
    let has_input_group = input_gid.is_some_and(|gid| {
        groups
            .lines()
            .find(|line| line.starts_with("Groups:"))
            .is_some_and(|line| {
                line.split_whitespace()
                    .skip(1)
                    .any(|value| value.parse::<u32>() == Ok(gid))
            })
    });
    println!(
        "Broad input group membership: {}",
        if has_input_group {
            "present; Soberized does not require it"
        } else {
            "not present"
        }
    );
    println!(
        "Gamepad option: enable it in Sober Settings (flatpak run org.vinegarhq.Sober config)."
    );
    println!("Background compatibility: requires the host validation in docs/VALIDATION.md.");
    if sandbox_test {
        let path = if Path::new(HELPER_PATH).exists() {
            HELPER_PATH.into()
        } else {
            std::env::current_exe()?.with_file_name("soberized-helper")
        };
        let status = Command::new(path)
            .arg("--self-test")
            .stdin(Stdio::null())
            .status()?;
        if !status.success() {
            return Err(io::Error::other("syscall sandbox self-test failed"));
        }
        let path = if Path::new(HELPER_PATH).exists() {
            HELPER_PATH.into()
        } else {
            std::env::current_exe()?.with_file_name("soberized-helper")
        };
        let status = Command::new(path)
            .arg("--self-test-ipc")
            .stdin(Stdio::null())
            .status()?;
        if !status.success() {
            return Err(io::Error::other("local IPC self-test failed"));
        }
    }
    if !ready {
        return Err(io::Error::other(
            "Soberized is not ready; resolve the issues reported above",
        ));
    }
    Ok(())
}
