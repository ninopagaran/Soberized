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
            "Soberized {}\n\nUsage: soberized <command>\n\n  doctor [--sandbox-test]  Check installation and optional syscall self-test\n  launch                   Launch Sober with session-only controller access, then start\n  start                    Arm input for the current Sober instance\n  stop                     Stop input and remove the virtual controller\n  toggle                   Toggle the session controller (for a Sway shortcut)\n  status                   Show the controller and next scheduled pulse\n  test-input               Queue one brief movement pulse in 10 seconds\n\nPulses can move your avatar. Park in a safe location before starting.\nEnable gamepad access in Sober Settings before use. Stop before playing other games.\nBackground anti-idle compatibility is experimental; see README.md.",
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
                "the helper has no active session; start Soberized before testing input",
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
            return start();
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(io::Error::other(
        "Sober is still starting; run soberized start after its window opens",
    ))
}

fn session() -> io::Result<()> {
    installed()?;
    let target = sober::instance()?.ok_or_else(|| io::Error::other("Sober is not running"))?;
    let mut monitor =
        roblox_log::Monitor::attach(roblox_log::default_path()?, sober::started_at(&target)?)?;
    notify::status("Waiting for a Roblox game join; no movement pulses yet")?;
    loop {
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
    let client = Client::connect(Path::new(SOCKET_PATH))?;
    if client.command(Operation::Arm)?.code != ReplyCode::Ok {
        return Err(io::Error::other(
            "another controller already owns the input lease",
        ));
    }
    notify::status("Movement pulses active; monitoring Roblox disconnects")?;
    eprintln!(
        "Soberized armed for Sober instance {}. Movement pulse: 250 ms at 50% left-stick deflection every 10 minutes; use a safe location and stop before other games.",
        target.id
    );
    let mut last_check = helper::boot_ms()?;
    let mut next_renewal = last_check + 10_000;
    let mut transition_since = None;
    loop {
        std::thread::sleep(Duration::from_secs(2));
        let current = sober::instance()?;
        let now = helper::boot_ms()?;
        if now.saturating_sub(last_check) > 5_000 {
            eprintln!(
                "Soberized stopped after suspend or a scheduling gap. Restart explicitly to resume."
            );
            return Ok(());
        }
        last_check = now;
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
            let pulses = client
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
        if now >= next_renewal {
            let response = client.command(Operation::Renew)?;
            if response.code != ReplyCode::Ok || response.state == State::Ready {
                return Err(io::Error::other(
                    "helper no longer holds this session lease",
                ));
            }
            next_renewal = now + 10_000;
        }
    }
}

fn doctor(sandbox_test: bool) -> io::Result<()> {
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
