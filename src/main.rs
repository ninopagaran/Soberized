use soberized::ipc::Client;
use soberized::protocol::{Command as Operation, ReplyCode, State};
use soberized::{APP_ID, HELPER_PATH, HELPER_UNIT, SOCKET_PATH, USER_UNIT, helper, sober};
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
            "Soberized {}\n\nUsage: soberized <command>\n\n  doctor [--sandbox-test]  Check installation and optional syscall self-test\n  launch                   Launch Sober with session-only controller access, then start\n  start                    Arm input for the current Sober instance\n  stop                     Stop input and remove the virtual controller\n  toggle                   Toggle the session controller (for a Sway shortcut)\n  status                   Show the controller and next scheduled pulse\n\nEnable gamepad access in Sober Settings before use. Stop before playing other games.\nBackground anti-idle compatibility is experimental; see README.md.",
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
    let metadata = std::fs::symlink_metadata(SOCKET_PATH).map_err(|_| {
        io::Error::other(
            "helper socket is unavailable; install Soberized and enable soberized-helper.socket",
        )
    })?;
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
    println!("Session controller started. Use soberized status to check that input is armed.");
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
    let controller = unit_active(true, USER_UNIT);
    if !unit_active(false, HELPER_UNIT) {
        println!(
            "{}",
            if controller {
                "Session controller is starting; check its journal if this persists."
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
            "Armed. Next pulse in {}s; {} pulses sent.",
            response.next_seconds, response.pulses
        ),
        State::Pulsing => println!(
            "Sending a bounded controller pulse; {} pulses sent.",
            response.pulses
        ),
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
    let client = Client::connect(Path::new(SOCKET_PATH))?;
    if client.command(Operation::Arm)?.code != ReplyCode::Ok {
        return Err(io::Error::other(
            "another controller already owns the input lease",
        ));
    }
    eprintln!(
        "Soberized armed for Sober instance {}. Pulse interval: 10 minutes; stop before other games.",
        target.id
    );
    let mut last_check = helper::boot_ms()?;
    let mut next_renewal = last_check + 10_000;
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
        Err(_) => {
            println!("Helper socket: unavailable (installation required)");
            ready = false;
        }
    }
    match sober::instance() {
        Ok(Some(_)) => println!("Sober instance: running"),
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
            "installation is incomplete; see README.md",
        ));
    }
    Ok(())
}
