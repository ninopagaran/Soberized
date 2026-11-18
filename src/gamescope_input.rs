//! Input only to the Gamescope process that launched the bound Sober instance.
//! Never use a desktop portal, a global pointer device, keyboard events, or shells.
use crate::sober::Instance;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::io;
use std::os::fd::{AsRawFd, IntoRawFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct Endpoint {
    pub display: String,
    runtime: PathBuf,
    pid: u32,
}

pub fn runtime_dir() -> io::Result<PathBuf> {
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    let path = PathBuf::from(format!("/run/user/{uid}"));
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err(io::Error::other(
            "a private user runtime directory is required",
        ));
    }
    Ok(path)
}

fn parent(pid: u32) -> io::Result<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    stat.rsplit_once(')')
        .and_then(|(_, fields)| fields.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| io::Error::other("invalid process ancestry"))
}

impl Endpoint {
    pub fn locate(target: &Instance) -> io::Result<Self> {
        let runtime = runtime_dir()?;
        let mut ancestors = Vec::new();
        let mut pid = target.pid;
        for _ in 0..32 {
            if pid <= 1 || ancestors.contains(&pid) {
                break;
            }
            ancestors.push(pid);
            pid = parent(pid)?;
        }
        let mut found = None;
        let mut candidates = 0;
        for entry in std::fs::read_dir(&runtime)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(display) = name.to_str().and_then(input_display) else {
                continue;
            };
            candidates += 1;
            if candidates > 16 {
                return Err(io::Error::other("too many Gamescope input sockets"));
            }
            let metadata = std::fs::symlink_metadata(entry.path())?;
            // SAFETY: geteuid has no preconditions.
            if !metadata.file_type().is_socket() || metadata.uid() != unsafe { libc::geteuid() } {
                continue;
            }
            let stream = match UnixStream::connect(entry.path()) {
                Ok(stream) => stream,
                Err(_) => continue,
            };
            let (peer_pid, peer_uid) = peer(&stream)?;
            if !ancestors.contains(&peer_pid) || peer_uid != metadata.uid() {
                continue;
            }
            let comm = std::fs::read_to_string(format!("/proc/{peer_pid}/comm"))?;
            if !matches!(comm.trim(), "gamescope" | "gamescope-wl") {
                continue;
            }
            if found.is_some() {
                return Err(io::Error::other("ambiguous Gamescope ancestry"));
            }
            found = Some(Self {
                display: display.to_owned(),
                runtime: runtime.clone(),
                pid: peer_pid,
            });
        }
        found.ok_or_else(|| {
            io::Error::other("auto-reconnect requires Sober launched inside Gamescope")
        })
    }

    pub fn verify(&self, target: &Instance) -> io::Result<()> {
        let current = Self::locate(target)?;
        if current.pid != self.pid || current.display != self.display {
            return Err(io::Error::other(
                "Gamescope instance changed; reconnect cancelled",
            ));
        }
        Ok(())
    }

    fn connect(&self, input: bool) -> io::Result<UnixStream> {
        let name = if input {
            format!("{}-ei", self.display)
        } else {
            self.display.clone()
        };
        let stream = UnixStream::connect(self.runtime.join(name))?;
        let (pid, uid) = peer(&stream)?;
        // SAFETY: geteuid has no preconditions.
        if pid != self.pid || uid != unsafe { libc::geteuid() } {
            return Err(io::Error::other("Gamescope socket credentials changed"));
        }
        Ok(stream)
    }

    pub fn capture_command(&self, path: &Path) -> io::Result<std::process::Command> {
        // Validate the compositor endpoint as well as its input endpoint.
        self.connect(false)?;
        let mut command = std::process::Command::new("/usr/bin/gamescopectl");
        command
            .env("GAMESCOPE_WAYLAND_DISPLAY", &self.display)
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env_remove("WAYLAND_SOCKET")
            .arg("screenshot")
            .arg(path)
            .arg("1");
        Ok(command)
    }

    pub fn pointer(&self) -> io::Result<Pointer> {
        Pointer::new(self.connect(true)?)
    }
}

fn input_display(name: &str) -> Option<&str> {
    let display = name.strip_suffix("-ei")?;
    let number = display.strip_prefix("gamescope-")?;
    (!number.is_empty() && number.len() <= 5 && number.bytes().all(|b| b.is_ascii_digit()))
        .then_some(display)
}

fn peer(stream: &UnixStream) -> io::Result<(u32, u32)> {
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: live socket and correctly sized initialized output storage.
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if length as usize != std::mem::size_of::<libc::ucred>() || credentials.pid <= 0 {
        return Err(io::Error::other("invalid Gamescope peer credentials"));
    }
    Ok((credentials.pid as u32, credentials.uid))
}

// Native sender API, restricted here to absolute pointer and left button.
#[link(name = "ei")]
unsafe extern "C" {
    fn ei_new_sender(data: *mut c_void) -> *mut c_void;
    fn ei_configure_name(context: *mut c_void, name: *const c_char);
    fn ei_setup_backend_fd(context: *mut c_void, fd: c_int) -> c_int;
    fn ei_unref(context: *mut c_void) -> *mut c_void;
    fn ei_dispatch(context: *mut c_void);
    fn ei_now(context: *mut c_void) -> u64;
    fn ei_get_event(context: *mut c_void) -> *mut c_void;
    fn ei_event_unref(event: *mut c_void) -> *mut c_void;
    fn ei_event_get_type(event: *mut c_void) -> c_int;
    fn ei_event_get_seat(event: *mut c_void) -> *mut c_void;
    fn ei_seat_bind_capabilities(seat: *mut c_void, ...);
    fn ei_event_get_device(event: *mut c_void) -> *mut c_void;
    fn ei_device_ref(device: *mut c_void) -> *mut c_void;
    fn ei_device_unref(device: *mut c_void) -> *mut c_void;
    fn ei_device_get_name(device: *mut c_void) -> *const c_char;
    fn ei_device_has_capability(device: *mut c_void, cap: c_int) -> bool;
    fn ei_device_start_emulating(device: *mut c_void, sequence: u32);
    fn ei_device_stop_emulating(device: *mut c_void);
    fn ei_device_pointer_motion_absolute(device: *mut c_void, x: f64, y: f64);
    fn ei_device_button_button(device: *mut c_void, button: u32, pressed: bool);
    fn ei_device_frame(device: *mut c_void, time: u64);
}

pub struct Pointer {
    context: NonNull<c_void>,
    device: Option<NonNull<c_void>>,
    ready: bool,
    pressed: bool,
    emulating: bool,
}

impl Pointer {
    fn new(stream: UnixStream) -> io::Result<Self> {
        // SAFETY: create one sender context, with no callbacks or user data.
        let context = NonNull::new(unsafe { ei_new_sender(std::ptr::null_mut()) })
            .ok_or_else(|| io::Error::other("could not allocate Gamescope input sender"))?;
        let mut pointer = Self {
            context,
            device: None,
            ready: false,
            pressed: false,
            emulating: false,
        };
        // SAFETY: live owned context, constant name. libei takes ownership of fd,
        // including on setup failure, and closes it during ei_unref.
        let result = unsafe {
            ei_configure_name(context.as_ptr(), c"Soberized reconnect".as_ptr());
            ei_setup_backend_fd(context.as_ptr(), stream.into_raw_fd())
        };
        if result < 0 {
            return Err(io::Error::from_raw_os_error(-result));
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        while !pointer.ready {
            pointer.dispatch()?;
            if Instant::now() >= deadline {
                return Err(io::Error::other("Gamescope pointer handshake timed out"));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(pointer)
    }

    fn dispatch(&mut self) -> io::Result<()> {
        // SAFETY: all objects belong to the owned context. Each event is
        // released exactly once; retained devices have an explicit reference.
        unsafe {
            ei_dispatch(self.context.as_ptr());
            for _ in 0..128 {
                let event = ei_get_event(self.context.as_ptr());
                if event.is_null() {
                    return Ok(());
                }
                let kind = ei_event_get_type(event);
                let device = ei_event_get_device(event);
                match kind {
                    2 => {
                        self.ready = false;
                        self.emulating = false;
                        self.pressed = false;
                        ei_event_unref(event);
                        return Err(io::Error::other("Gamescope input disconnected"));
                    }
                    3 => {
                        ei_seat_bind_capabilities(
                            ei_event_get_seat(event),
                            2 as c_int,
                            32 as c_int,
                            std::ptr::null::<c_void>(),
                        );
                    }
                    5 if self.device.is_none() => {
                        let name = ei_device_get_name(device);
                        if !name.is_null()
                            && CStr::from_ptr(name) == c"Gamescope Virtual Input"
                            && ei_device_has_capability(device, 2)
                            && ei_device_has_capability(device, 32)
                        {
                            self.device = NonNull::new(ei_device_ref(device));
                        }
                    }
                    6 | 7 if self.device.is_some_and(|d| d.as_ptr() == device) => {
                        self.ready = false;
                        self.emulating = false;
                        self.pressed = false;
                        ei_event_unref(event);
                        return Err(io::Error::other("Gamescope pointer was removed or paused"));
                    }
                    8 if self.device.is_some_and(|d| d.as_ptr() == device) => self.ready = true,
                    _ => {}
                }
                ei_event_unref(event);
            }
        }
        Err(io::Error::other("too many Gamescope input events"))
    }

    pub fn click(&mut self, x: u32, y: u32) -> io::Result<()> {
        self.dispatch()?;
        if !self.ready || x >= 4096 || y >= 2160 {
            return Err(io::Error::other("invalid reconnect click target"));
        }
        let device = self.device.unwrap().as_ptr();
        // SAFETY: sender device is resumed and has the required capabilities.
        // The only button code permitted by this implementation is BTN_LEFT.
        unsafe {
            ei_device_start_emulating(device, 1);
            ei_device_pointer_motion_absolute(device, f64::from(x), f64::from(y));
            ei_device_frame(device, ei_now(self.context.as_ptr()));
        }
        self.emulating = true;
        self.dispatch()?;
        std::thread::sleep(Duration::from_millis(50));
        self.pressed = true;
        unsafe {
            ei_device_button_button(device, 0x110, true);
            ei_device_frame(device, ei_now(self.context.as_ptr()));
        }
        self.dispatch()?;
        std::thread::sleep(Duration::from_millis(50));
        self.release();
        self.dispatch()
    }

    fn release(&mut self) {
        if !self.emulating {
            return;
        }
        if let Some(device) = self.device {
            // SAFETY: the explicit reference remains live until Drop.
            unsafe {
                if self.pressed {
                    ei_device_button_button(device.as_ptr(), 0x110, false);
                    ei_device_frame(device.as_ptr(), ei_now(self.context.as_ptr()));
                }
                ei_device_stop_emulating(device.as_ptr());
                ei_dispatch(self.context.as_ptr());
            }
        }
        self.pressed = false;
        self.emulating = false;
    }
}

impl Drop for Pointer {
    fn drop(&mut self) {
        self.release();
        // SAFETY: release the uniquely owned device reference and context.
        unsafe {
            if let Some(device) = self.device {
                ei_device_unref(device.as_ptr());
            }
            ei_unref(self.context.as_ptr());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_fixed_gamescope_socket_names_are_accepted() {
        assert_eq!(input_display("gamescope-0-ei"), Some("gamescope-0"));
        for name in [
            "wayland-0-ei",
            "../gamescope-0-ei",
            "gamescope-x-ei",
            "gamescope--ei",
            "gamescope-123456-ei",
        ] {
            assert_eq!(input_display(name), None);
        }
    }

    #[test]
    #[ignore = "requires host AF_UNIX access; run scripts/check-host.sh"]
    fn peer_credentials_come_from_the_kernel() {
        let (a, _b) = UnixStream::pair().unwrap();
        let (pid, uid) = peer(&a).unwrap();
        assert_eq!(pid, std::process::id());
        assert_eq!(uid, unsafe { libc::geteuid() });
    }
}
