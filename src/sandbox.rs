//! A second, irreversible sandbox installed after trusted device initialization.
use std::ffi::{CString, c_void};
use std::io;

#[link(name = "seccomp")]
unsafe extern "C" {
    fn seccomp_init(default_action: u32) -> *mut c_void;
    fn seccomp_release(context: *mut c_void);
    fn seccomp_syscall_resolve_name(name: *const libc::c_char) -> libc::c_int;
    fn seccomp_rule_add(
        context: *mut c_void,
        action: u32,
        syscall: libc::c_int,
        argument_count: libc::c_uint,
        ...
    ) -> libc::c_int;
    fn seccomp_load(context: *mut c_void) -> libc::c_int;
}

// No open/openat, ioctl, fcntl, socket, connect, exec, clone/fork, ptrace,
// pidfd_getfd, io_uring, or sendmsg/recvmsg (descriptor transfer). The native
// architecture is the only libseccomp architecture added to the filter.
const ALLOWED: &[&str] = &[
    "read",
    "write",
    "writev",
    "close",
    "poll",
    "ppoll",
    "accept4",
    "getsockopt",
    "sendto",
    "recvfrom",
    "clock_gettime",
    "clock_nanosleep",
    "nanosleep",
    "brk",
    "mmap",
    "mprotect",
    "mremap",
    "munmap",
    "madvise",
    "futex",
    "rt_sigaction",
    "rt_sigprocmask",
    "rt_sigreturn",
    "sigaltstack",
    "getpid",
    "gettid",
    "getuid",
    "geteuid",
    "getgid",
    "getegid",
    "sched_yield",
    "restart_syscall",
    "exit",
    "exit_group",
];

pub fn lock_down() -> io::Result<()> {
    // SAFETY: these libseccomp calls receive a live context and NUL-terminated
    // constant syscall names. No filter rule has variadic comparison arguments.
    unsafe {
        if libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) != 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err(io::Error::last_os_error());
        }
        let context = seccomp_init(0x0005_0000 | libc::EPERM as u32);
        if context.is_null() {
            return Err(io::Error::other("could not allocate syscall filter"));
        }
        let result = (|| {
            for name in ALLOWED {
                let name = CString::new(*name).unwrap();
                let number = seccomp_syscall_resolve_name(name.as_ptr());
                if number < 0 {
                    return Err(io::Error::other("unknown required syscall"));
                }
                let result = seccomp_rule_add(context, 0x7fff_0000, number, 0);
                if result < 0 {
                    return Err(io::Error::from_raw_os_error(-result));
                }
            }
            let result = seccomp_load(context);
            if result < 0 {
                return Err(io::Error::from_raw_os_error(-result));
            }
            Ok(())
        })();
        seccomp_release(context);
        result
    }
}

/// Runs only in a disposable process: seccomp cannot be undone.
pub fn self_test() -> io::Result<()> {
    // This path deliberately needs no sockets or input devices, so it can also
    // verify the installed filter inside restricted build environments.
    let _readable_before_filter = std::fs::File::open("/dev/null")?;
    lock_down()?;
    // SAFETY: each deliberately forbidden call receives valid or inert arguments.
    // They must fail with EPERM before the kernel executes the operation.
    unsafe {
        macro_rules! denied {
            ($label:literal, $call:expr) => {{
                let result = $call;
                if result != -1 || *libc::__errno_location() != libc::EPERM {
                    return Err(io::Error::other(concat!("sandbox did not deny ", $label)));
                }
            }};
        }
        denied!(
            "previously accessible files",
            libc::syscall(
                libc::SYS_openat,
                libc::AT_FDCWD,
                c"/dev/null".as_ptr(),
                libc::O_RDONLY,
                0
            )
        );
        denied!(
            "home files",
            libc::syscall(
                libc::SYS_openat,
                libc::AT_FDCWD,
                c"/home".as_ptr(),
                libc::O_RDONLY,
                0
            )
        );
        denied!(
            "physical input devices",
            libc::syscall(
                libc::SYS_openat,
                libc::AT_FDCWD,
                c"/dev/input".as_ptr(),
                libc::O_RDONLY,
                0
            )
        );
        denied!(
            "uinput",
            libc::syscall(
                libc::SYS_openat,
                libc::AT_FDCWD,
                c"/dev/uinput".as_ptr(),
                libc::O_RDWR,
                0
            )
        );
        denied!(
            "keyboard reconfiguration",
            libc::syscall(libc::SYS_ioctl, -1, 0x4004_5565u64, 30)
        );
        denied!(
            "network sockets",
            libc::syscall(libc::SYS_socket, libc::AF_INET, libc::SOCK_STREAM, 0)
        );
        denied!(
            "local sockets",
            libc::syscall(libc::SYS_socket, libc::AF_UNIX, libc::SOCK_STREAM, 0)
        );
        denied!(
            "program execution",
            libc::syscall(
                libc::SYS_execve,
                c"/soberized-nonexistent".as_ptr(),
                std::ptr::null::<libc::c_void>(),
                std::ptr::null::<libc::c_void>()
            )
        );
        denied!(
            "descriptor transfer",
            libc::syscall(libc::SYS_sendmsg, -1, std::ptr::null::<libc::c_void>(), 0)
        );
        denied!(
            "io_uring",
            libc::syscall(
                libc::SYS_io_uring_setup,
                0,
                std::ptr::null::<libc::c_void>()
            )
        );
        denied!("descriptor duplication", libc::syscall(libc::SYS_dup, -1));
    }
    println!(
        "PASS: syscall filter blocks file access, device reconfiguration, sockets, execution, descriptor transfer, io_uring, and duplication"
    );
    Ok(())
}

pub fn ipc_self_test() -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let (left, right) = crate::ipc::socket_pair()?;
    let mut buffer = [0; crate::protocol::REQUEST_LEN];
    lock_down()?;
    crate::ipc::authorize(left.as_raw_fd(), unsafe { libc::geteuid() })?;
    crate::ipc::send_packet(left.as_raw_fd(), &crate::protocol::Command::Status.encode())?;
    let size = crate::ipc::receive_packet(right.as_raw_fd(), &mut buffer, false)?;
    if size != buffer.len()
        || crate::protocol::Command::decode(&buffer)? != crate::protocol::Command::Status
    {
        return Err(io::Error::other(
            "local protocol failed under the syscall filter",
        ));
    }
    println!("PASS: peer credentials and local packets work under the installed syscall filter");
    Ok(())
}
