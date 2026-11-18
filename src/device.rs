//! The only device construction code. No physical input devices are opened.
use std::ffi::{c_char, c_int, c_uint, c_void};
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::ptr::NonNull;

pub const NAME: &str = "Soberized Virtual Gamepad";
const EV_SYN: u32 = 0;
const EV_KEY: u32 = 1;
const EV_ABS: u32 = 3;
const ABS_X: u32 = 0;
const BTN_SELECT: u32 = 0x13a;
const BUTTONS: &[u32] = &[
    0x130, 0x131, 0x133, 0x134, 0x136, 0x137, 0x13a, 0x13b, 0x13c, 0x13d, 0x13e,
];
const AXES: &[(u32, i32, i32, i32)] = &[
    (0, -32768, 32767, 4096),
    (1, -32768, 32767, 4096),
    (3, -32768, 32767, 4096),
    (4, -32768, 32767, 4096),
    (2, 0, 255, 0),
    (5, 0, 255, 0),
    (16, -1, 1, 0),
    (17, -1, 1, 0),
];

#[link(name = "evdev")]
unsafe extern "C" {
    fn libevdev_new() -> *mut c_void;
    fn libevdev_free(device: *mut c_void);
    fn libevdev_set_name(device: *mut c_void, name: *const c_char);
    fn libevdev_set_id_vendor(device: *mut c_void, vendor: c_int);
    fn libevdev_set_id_product(device: *mut c_void, product: c_int);
    fn libevdev_set_id_bustype(device: *mut c_void, bus: c_int);
    fn libevdev_set_id_version(device: *mut c_void, version: c_int);
    fn libevdev_enable_event_type(device: *mut c_void, event_type: c_uint) -> c_int;
    fn libevdev_enable_event_code(
        device: *mut c_void,
        event_type: c_uint,
        code: c_uint,
        data: *const c_void,
    ) -> c_int;
    fn libevdev_uinput_create_from_device(
        device: *const c_void,
        fd: c_int,
        output: *mut *mut c_void,
    ) -> c_int;
    fn libevdev_uinput_destroy(device: *mut c_void);
    fn libevdev_uinput_write_event(
        device: *const c_void,
        event_type: c_uint,
        code: c_uint,
        value: c_int,
    ) -> c_int;
}

fn checked(result: c_int) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::from_raw_os_error(-result))
    } else {
        Ok(())
    }
}

pub trait Output {
    fn pulse(&mut self, amplitude: i32) -> io::Result<()>;
    fn select_ui(&mut self) -> io::Result<()>;
    fn neutral(&mut self) -> io::Result<()>;
}

pub struct Gamepad {
    device: NonNull<c_void>,
    // Keep unique ownership until after libevdev frees its userspace bookkeeping.
    _fd: OwnedFd,
}

impl Gamepad {
    pub fn create(fd: OwnedFd) -> io::Result<Self> {
        // SAFETY: initialized stat storage and a live descriptor.
        unsafe {
            let mut stat: libc::stat = std::mem::zeroed();
            if libc::fstat(fd.as_raw_fd(), &mut stat) != 0 {
                return Err(io::Error::last_os_error());
            }
            if stat.st_mode & libc::S_IFMT != libc::S_IFCHR
                || libc::major(stat.st_rdev) != 10
                || libc::minor(stat.st_rdev) != 223
            {
                return Err(io::Error::other(
                    "uinput descriptor is not the Linux uinput character device",
                ));
            }
        }
        let descriptor = Descriptor::new()?;
        let mut raw = std::ptr::null_mut();
        // SAFETY: descriptor and fd are live, output points to a pointer-sized slot.
        unsafe {
            checked(libevdev_uinput_create_from_device(
                descriptor.0.as_ptr(),
                fd.as_raw_fd(),
                &mut raw,
            ))?;
        }
        let device =
            NonNull::new(raw).ok_or_else(|| io::Error::other("libevdev returned no controller"))?;
        let mut gamepad = Self { device, _fd: fd };
        gamepad.neutral()?;
        Ok(gamepad)
    }

    fn event(&self, event_type: u32, code: u32, value: i32) -> io::Result<()> {
        // SAFETY: this pointer is uniquely owned and remains live until Drop.
        unsafe {
            checked(libevdev_uinput_write_event(
                self.device.as_ptr(),
                event_type,
                code,
                value,
            ))
        }
    }
}

impl Output for Gamepad {
    fn select_ui(&mut self) -> io::Result<()> {
        self.event(EV_KEY, BTN_SELECT, 1)?;
        self.event(EV_SYN, 0, 0)
    }

    fn pulse(&mut self, amplitude: i32) -> io::Result<()> {
        if amplitude != crate::schedule::AMPLITUDE && amplitude != -crate::schedule::AMPLITUDE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported controller pulse",
            ));
        }
        self.event(EV_ABS, ABS_X, amplitude)?;
        self.event(EV_SYN, 0, 0)
    }

    fn neutral(&mut self) -> io::Result<()> {
        for &(axis, _, _, _) in AXES {
            self.event(EV_ABS, axis, 0)?;
        }
        for &button in BUTTONS {
            self.event(EV_KEY, button, 0)?;
        }
        self.event(EV_SYN, 0, 0)
    }
}

impl Drop for Gamepad {
    fn drop(&mut self) {
        let _ = self.neutral();
        // SAFETY: free the uniquely owned libevdev object. After lockdown its
        // UI_DEV_DESTROY ioctl intentionally receives EPERM. Closing _fd below
        // performs the actual kernel destruction, including on process death.
        unsafe {
            libevdev_uinput_destroy(self.device.as_ptr());
        }
    }
}

struct Descriptor(NonNull<c_void>);

impl Descriptor {
    fn new() -> io::Result<Self> {
        // SAFETY: a fresh libevdev object with no attached physical fd; all
        // capabilities are constants. No user data reaches these FFI calls.
        unsafe {
            let descriptor = Self(
                NonNull::new(libevdev_new())
                    .ok_or_else(|| io::Error::other("could not allocate gamepad descriptor"))?,
            );
            let device = descriptor.0.as_ptr();
            libevdev_set_name(device, c"Soberized Virtual Gamepad".as_ptr());
            libevdev_set_id_bustype(device, 3); // BUS_USB, Xbox-compatible mapping.
            libevdev_set_id_vendor(device, 0x045e);
            libevdev_set_id_product(device, 0x028e);
            libevdev_set_id_version(device, 0x0110);
            checked(libevdev_enable_event_type(device, EV_KEY))?;
            checked(libevdev_enable_event_type(device, EV_ABS))?;
            for &button in BUTTONS {
                checked(libevdev_enable_event_code(
                    device,
                    EV_KEY,
                    button,
                    std::ptr::null(),
                ))?;
            }
            for &(axis, minimum, maximum, flat) in AXES {
                let info = libc::input_absinfo {
                    value: 0,
                    minimum,
                    maximum,
                    fuzz: 0,
                    flat,
                    resolution: 0,
                };
                checked(libevdev_enable_event_code(
                    device,
                    EV_ABS,
                    axis,
                    (&info as *const libc::input_absinfo).cast(),
                ))?;
            }
            Ok(descriptor)
        }
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: this descriptor is uniquely owned and is no longer used.
        unsafe {
            libevdev_free(self.0.as_ptr());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[link(name = "evdev")]
    unsafe extern "C" {
        fn libevdev_has_event_code(
            device: *const c_void,
            event_type: c_uint,
            code: c_uint,
        ) -> c_int;
    }

    #[test]
    fn descriptor_has_only_fixed_gamepad_capabilities() {
        let descriptor = Descriptor::new().unwrap();
        // Check all Linux key codes, including keyboard, mouse, and media keys.
        for code in 0..=0x2ff {
            let expected = i32::from(BUTTONS.contains(&code));
            assert_eq!(
                unsafe { libevdev_has_event_code(descriptor.0.as_ptr(), EV_KEY, code) },
                expected,
                "key code {code}"
            );
        }
        for code in 0..=0x3f {
            assert_eq!(
                unsafe { libevdev_has_event_code(descriptor.0.as_ptr(), EV_ABS, code) },
                i32::from(AXES.iter().any(|a| a.0 == code))
            );
        }
        for code in 0..16 {
            assert_eq!(
                unsafe { libevdev_has_event_code(descriptor.0.as_ptr(), 2, code) },
                0,
                "relative mouse code {code}"
            );
        }
    }
}
