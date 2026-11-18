use soberized::ipc::Client;
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
            "Soberized {}\n\nUsage: soberized <command>\n\n  doctor [--sandbox-test]  Check installation and optional syscall self-test\n  launch                   Prepare a neutral gamepad, then launch Sober\n  start                    Arm input for the current Sober instance\n  stop                     Stop input and remove the virtual controller\n  toggle                   Toggle the session controller (for a Sway shortcut)\n  status                   Show the controller and next scheduled pulse\n  test-input               Queue one brief movement pulse in 10 seconds\n\nPulses can move your avatar. Park in a safe location before starting.\nEnable gamepad access in Sober Settings before use. Stop before playing other games.\nBackground anti-idle compatibility is experimental; see README.md.",
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
    if arguments.len() != 1 {
        return Err(io::Error::other("unknown arguments; use soberized --help"));
    }
    match arguments[0].as_str() {
        "launch" => launch(),
        "start" => start(),
        "stop" => stop(),
        "toggle" if unit_active(true, USER_UNIT) => stop(),
        "toggle" => start(),
        "status" => status(),
        "test-input" => test_input(),
        "session" => session(), // Invoked by the root-owned systemd user unit.
        _ => Err(io::Error::other("unknown command; use soberized --help")),
    }
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

fn start() -> io::Result<()> {
    installed()?;
    if sober::instance()?.is_none() {
        return Err(io::Error::other(
            "Sober is not running; use soberized launch",
        ));
    }
    control_service("start")?;
    wait_for_controller(false)?;
    println!(
        "Session controller started. Pulses briefly move your avatar; use a safe location. Check soberized status to confirm input is armed."
    );
    Ok(())
}

fn stop() -> io::Result<()> {
    control_service("stop")?;
    if unit_active(false, HELPER_UNIT) {
        // The worker usually closes its lease first. Tolerate that cleanup race.
        if let Ok(client) = Client::connect(Path::new(SOCKET_PATH)) {
            let _ = client.command(Operation::Stop);
        }
    }
    println!("Stopped.");
    Ok(())
}

fn status() -> io::Result<()> {
    println!("Soberized {}", env!("CARGO_PKG_VERSION"));
    installed()?;
    let session = units::query(true, USER_UNIT)?;
    if session.failed() {
        if session.status.starts_with("Roblox disconnected:") {
            return Err(io::Error::other(session.status));
        }
        return Err(io::Error::other(format!(
            "session controller failed ({}): {}; inspect journalctl --user -u {USER_UNIT} -b",
            session.result, session.status
        )));
    }
    let controller = unit_active(true, USER_UNIT);
    if !unit_active(false, HELPER_UNIT) {
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
    }
    let response = Client::connect(Path::new(SOCKET_PATH))?.command(Operation::Status)?;
    match response.state {
        State::Ready => println!("Waiting for a session lease; no pulses scheduled."),
        State::Prepared => println!(
            "{}",
            if session.status.starts_with("Waiting for ") {
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

fn launch() -> io::Result<()> {
    installed()?;
    if sober::instance()?.is_some() {
        return Err(io::Error::other(
            "Sober is already running; close it before launching with session-only gamepad permissions, or use start for an already prepared instance",
        ));
    }
    // The worker owns a neutral-only lease before Flatpak starts and
    // Sober initializes its controller handling. It arms only after a game join.
    control_service("restart")?;
    let result = launch_prepared();
    if result.is_err() {
        let _ = control_service("stop");
    }
    result
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
        }
    }
    Err(io::Error::other(
        "virtual controller preparation timed out; inspect the helper and session journals",
    ))
}

fn launch_prepared() -> io::Result<()> {
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
    let mut child = Command::new("/usr/bin/flatpak")
        .args([
            "run",
            "--device=input",
            "--env=SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS=1",
            APP_ID,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()?;
    for _ in 0..30 {
        if let Some(exit) = child.try_wait()? {
            return Err(io::Error::other(format!(
                "Sober exited during launch ({exit})"
            )));
        }
        if sober::instance()?.is_some() {
            println!(
                "Sober launched with the virtual controller already present. Movement stays disabled until a Roblox game join; check soberized status."
            );
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(io::Error::other(
        "Sober startup timed out; the controller was stopped. Close Sober and retry soberized launch",
    ))
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

fn session() -> io::Result<()> {
    installed()?;
    eprintln!(
        "Soberized session {}: preparing a neutral controller",
        env!("CARGO_PKG_VERSION")
    );
    let mut lease = SessionLease::prepare()?;
    let startup_deadline = lease.last_check + 30_000;
    notify::status("Waiting for Sober startup; virtual controller prepared, movement disabled")?;
    let target = loop {
        let now = lease.check()?;
        if let Some(target) = sober::instance()? {
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
            let pulses = lease
                .client
                .command(Operation::Status)
                .ok()
                .map(|response| response.pulses);
            let message = event.disconnect_message(pulses).unwrap();
            notify::status(&message)?;
            // Dropping this owner connection removes the gamepad. Do not
            // navigate/rejoin automatically or continue reporting input active.
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
