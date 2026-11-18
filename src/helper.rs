use crate::device::{Gamepad, Output};
use crate::ipc;
use crate::protocol::{Command, REQUEST_LEN, ReplyCode};
use crate::schedule::{Effect, Schedule};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

const MAX_CLIENTS: usize = 8;
const CLIENT_IDLE_MS: u64 = 5_000;

pub fn boot_ms() -> io::Result<u64> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: time is valid output storage. BOOTTIME includes time suspended.
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut time) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((time.tv_sec as u64)
        .saturating_mul(1_000)
        .saturating_add(time.tv_nsec as u64 / 1_000_000))
}

fn activated_descriptors() -> io::Result<(OwnedFd, OwnedFd)> {
    let invalid = || {
        io::Error::other(
            "helper requires systemd activation with named control and uinput descriptors",
        )
    };
    let pid = std::env::var("LISTEN_PID")
        .ok()
        .and_then(|s| s.parse::<u32>().ok());
    let count = std::env::var("LISTEN_FDS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok());
    if pid != Some(std::process::id()) || count != Some(2) {
        return Err(invalid());
    }
    let names = std::env::var("LISTEN_FDNAMES").map_err(|_| invalid())?;
    let names: Vec<_> = names.split(':').collect();
    if names.len() != 2 || !names.contains(&"control") || !names.contains(&"uinput") {
        return Err(invalid());
    }
    let control = 3 + names.iter().position(|name| *name == "control").unwrap() as i32;
    let uinput = 3 + names.iter().position(|name| *name == "uinput").unwrap() as i32;
    // SAFETY: systemd passed these distinct descriptors to this process, and no
    // other OwnedFd owns them. Both are validated before use.
    let (control, uinput) =
        unsafe { (OwnedFd::from_raw_fd(control), OwnedFd::from_raw_fd(uinput)) };
    ipc::validate_listener(control.as_raw_fd())?;
    ipc::set_nonblocking(control.as_raw_fd())?;
    ipc::set_nonblocking(uinput.as_raw_fd())?;
    Ok((control, uinput))
}

pub fn run(owner_uid: u32) -> io::Result<()> {
    // SAFETY: geteuid has no preconditions.
    if !separate_identity(unsafe { libc::geteuid() }, owner_uid) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "helper must run under a separate unprivileged identity for a desktop user",
        ));
    }
    let (listener, uinput) = activated_descriptors()?;
    let mut gamepad = Gamepad::create(uinput)?;
    let schedule = Schedule::new(boot_ms()?);
    let clients = Vec::with_capacity(MAX_CLIENTS);
    // All capabilities, allocation, and trusted startup finish before any
    // accept/recv. Failure to install the sandbox removes the controller.
    crate::sandbox::lock_down()?;
    eprintln!("Soberized helper: fixed gamepad created; syscall sandbox installed");
    event_loop(listener, &mut gamepad, owner_uid, schedule, clients)
}

fn separate_identity(helper_uid: u32, owner_uid: u32) -> bool {
    helper_uid != 0 && helper_uid != owner_uid && (1_000..60_000).contains(&owner_uid)
}

struct Peer {
    fd: OwnedFd,
    id: u64,
    last_seen: u64,
    leased: bool,
}

fn apply_tick(schedule: &mut Schedule, output: &mut impl Output, now: u64) -> io::Result<bool> {
    match schedule.tick(now) {
        Effect::None => {}
        Effect::Pulse(amplitude, kind) => {
            output.pulse(amplitude)?;
            eprintln!(
                "Soberized pulse={} phase=deflect source={} axis=ABS_X value={} duration_ms={} boottime_ms={}",
                schedule.pulse_count(),
                kind.label(),
                amplitude,
                crate::schedule::PULSE_MS,
                now
            );
        }
        Effect::Neutral => {
            output.neutral()?;
            eprintln!(
                "Soberized pulse={} phase=neutral boottime_ms={}",
                schedule.pulse_count(),
                now
            );
        }
        Effect::Exit(reason) => {
            output.neutral()?;
            eprintln!(
                "Soberized stopped reason={} pulses={} boottime_ms={}",
                reason.label(),
                schedule.pulse_count(),
                now
            );
            return Ok(false);
        }
    }
    Ok(true)
}

fn event_loop(
    listener: OwnedFd,
    output: &mut impl Output,
    owner_uid: u32,
    mut schedule: Schedule,
    mut clients: Vec<Peer>,
) -> io::Result<()> {
    let mut next_id: u64 = 1;
    loop {
        let now = boot_ms()?;
        if !apply_tick(&mut schedule, output, now)? {
            return Ok(());
        }
        let mut fds = [libc::pollfd {
            fd: -1,
            events: libc::POLLIN,
            revents: 0,
        }; MAX_CLIENTS + 1];
        fds[0].fd = listener.as_raw_fd();
        let count = clients.len();
        for (index, client) in clients.iter().enumerate() {
            fds[index + 1].fd = client.fd.as_raw_fd();
        }
        // SAFETY: the initialized prefix contains all poll entries; timeout is bounded.
        let result = unsafe { libc::poll(fds.as_mut_ptr(), (count + 1) as libc::nfds_t, 50) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        let now = boot_ms()?;
        // Expiry and suspend detection precede renewal or any new pulse.
        if !apply_tick(&mut schedule, output, now)? {
            return Ok(());
        }
        for index in (0..count).rev() {
            let peer = &mut clients[index];
            let events = fds[index + 1].revents;
            let mut remove = events & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0;
            remove |= !peer.leased && now.saturating_sub(peer.last_seen) >= CLIENT_IDLE_MS;
            if events & libc::POLLIN != 0 && !remove {
                let mut bytes = [0; REQUEST_LEN];
                match ipc::receive_packet(peer.fd.as_raw_fd(), &mut bytes, true) {
                    Ok(REQUEST_LEN) => match Command::decode(&bytes) {
                        Ok(command) => {
                            peer.last_seen = now;
                            let response = schedule.command(peer.id, command, now);
                            if command == Command::Arm && response.code == ReplyCode::Ok {
                                peer.leased = true;
                            }
                            remove =
                                ipc::send_packet(peer.fd.as_raw_fd(), &response.encode()).is_err();
                        }
                        Err(_) => remove = true,
                    },
                    Ok(_) => remove = true,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(_) => remove = true,
                }
            }
            if remove {
                schedule.disconnect(peer.id);
                clients.swap_remove(index);
            }
        }
        // Stop or disconnect must take effect before accepting another client.
        if !apply_tick(&mut schedule, output, boot_ms()?)? {
            return Ok(());
        }
        if fds[0].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(io::Error::other("control listener failed"));
        }
        if fds[0].revents & libc::POLLIN != 0 {
            // Bound work and memory even when an authorized client floods connects.
            for _ in 0..MAX_CLIENTS {
                match ipc::accept(listener.as_raw_fd()) {
                    Ok(fd)
                        if clients.len() < MAX_CLIENTS
                            && ipc::authorize(fd.as_raw_fd(), owner_uid).is_ok() =>
                    {
                        clients.push(Peer {
                            fd,
                            id: next_id,
                            last_seen: now,
                            leased: false,
                        });
                        next_id = next_id
                            .checked_add(1)
                            .ok_or_else(|| io::Error::other("client identifier exhausted"))?;
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::{AMPLITUDE, INTERVAL_MS};

    #[derive(Default)]
    struct RecordingOutput {
        values: Vec<i32>,
    }

    #[test]
    fn helper_cannot_share_the_desktop_identity_or_run_as_root() {
        assert!(!separate_identity(0, 1_000));
        assert!(!separate_identity(1_000, 1_000));
        assert!(!separate_identity(61_184, 0));
        assert!(!separate_identity(61_184, 65_534));
        assert!(separate_identity(61_184, 1_000));
    }
    impl Output for RecordingOutput {
        fn pulse(&mut self, amplitude: i32) -> io::Result<()> {
            self.values.push(amplitude);
            Ok(())
        }
        fn neutral(&mut self) -> io::Result<()> {
            self.values.push(0);
            Ok(())
        }
    }

    #[test]
    fn output_is_centered_when_a_client_disappears_mid_pulse() {
        let mut s = Schedule::new(0);
        let mut output = RecordingOutput::default();
        s.command(1, Command::Arm, 0);
        for now in (0..=INTERVAL_MS).step_by(1_000) {
            s.command(1, Command::Renew, now);
            assert!(apply_tick(&mut s, &mut output, now).unwrap());
        }
        assert_eq!(output.values, [AMPLITUDE]);
        s.disconnect(1);
        assert!(!apply_tick(&mut s, &mut output, INTERVAL_MS + 1).unwrap());
        assert_eq!(output.values, [AMPLITUDE, 0]);
    }

    #[test]
    #[ignore = "requires host AF_UNIX access; run scripts/check-host.sh"]
    fn real_server_handles_ownership_bad_packets_and_stop() {
        use crate::ipc::Client;
        use crate::protocol::{Response, State};
        use std::os::unix::ffi::OsStrExt;
        let directory =
            std::env::temp_dir().join(format!("soberized-server-test-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(self.0.join("control.sock"));
                let _ = std::fs::remove_dir(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        let path = directory.join("control.sock");
        // SAFETY: construct a bounded test-only pathname socket. No real input
        // device is created; the production event loop uses RecordingOutput.
        let listener = unsafe {
            let fd = libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0);
            assert!(fd >= 0, "{}", io::Error::last_os_error());
            let fd = OwnedFd::from_raw_fd(fd);
            let mut address: libc::sockaddr_un = std::mem::zeroed();
            address.sun_family = libc::AF_UNIX as libc::sa_family_t;
            let bytes = path.as_os_str().as_bytes();
            assert!(bytes.len() < address.sun_path.len());
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                address.sun_path.as_mut_ptr().cast(),
                bytes.len(),
            );
            assert_eq!(
                libc::bind(
                    fd.as_raw_fd(),
                    (&address as *const libc::sockaddr_un).cast(),
                    std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t
                ),
                0,
                "socket bind failed: {}",
                io::Error::last_os_error()
            );
            assert_eq!(libc::listen(fd.as_raw_fd(), 8), 0);
            fd
        };
        ipc::validate_listener(listener.as_raw_fd()).unwrap();
        ipc::set_nonblocking(listener.as_raw_fd()).unwrap();
        let server = std::thread::spawn(move || {
            let mut output = RecordingOutput::default();
            let schedule = Schedule::new(boot_ms().unwrap());
            event_loop(
                listener,
                &mut output,
                unsafe { libc::geteuid() },
                schedule,
                Vec::with_capacity(MAX_CLIENTS),
            )
            .unwrap();
            output.values
        });
        let owner = Client::connect_for_test(&path).unwrap();
        let response = owner.command(Command::Arm).unwrap();
        assert_eq!(response.state, State::Armed);
        assert_eq!(response.next_seconds, 600);
        let other = Client::connect_for_test(&path).unwrap();
        assert_eq!(other.command(Command::Arm).unwrap().code, ReplyCode::Busy);
        assert_eq!(
            other.command(Command::Renew).unwrap().code,
            ReplyCode::NotOwner
        );
        for bytes in [
            vec![0; REQUEST_LEN],
            [Command::Arm.encode().as_slice(), &[0; 256]].concat(),
        ] {
            let malicious = Client::connect_for_test(&path).unwrap();
            ipc::send_packet(malicious.as_raw_fd(), &bytes).unwrap();
            let mut reply = [0; 16];
            let length = ipc::receive_packet(malicious.as_raw_fd(), &mut reply, false).unwrap();
            assert_eq!(length, 0, "malformed client should be disconnected");
        }
        assert_eq!(owner.command(Command::Status).unwrap().state, State::Armed);
        let Response { state, code, .. } = other.command(Command::Stop).unwrap();
        assert_eq!(state, State::Ready);
        assert_eq!(code, ReplyCode::Ok);
        assert_eq!(server.join().unwrap(), [0]);
    }
}
