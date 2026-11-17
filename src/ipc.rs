//! Linux-only local IPC. Message lengths and peer identities are checked at both ends.
use crate::protocol::{Command, RESPONSE_LEN, Response};
use std::io;
use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;

fn cvt(result: libc::c_int) -> io::Result<libc::c_int> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

pub fn socket_pair() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [-1; 2];
    // SAFETY: fds points to space for exactly two file descriptors.
    unsafe {
        cvt(libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        ))?;
        Ok((OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])))
    }
}

pub fn peer_uid(fd: RawFd) -> io::Result<u32> {
    // SAFETY: initialized output and correct size for Linux SO_PEERCRED.
    unsafe {
        let mut credentials: libc::ucred = zeroed();
        let mut size = size_of::<libc::ucred>() as libc::socklen_t;
        cvt(libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut size,
        ))?;
        if size as usize != size_of::<libc::ucred>() {
            return Err(io::Error::other("unexpected peer credential size"));
        }
        Ok(credentials.uid)
    }
}

pub fn authorize(fd: RawFd, owner_uid: u32) -> io::Result<()> {
    if peer_uid(fd)? != owner_uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unauthorized control client",
        ));
    }
    Ok(())
}

pub fn validate_listener(fd: RawFd) -> io::Result<()> {
    for (option, expected) in [
        (libc::SO_TYPE, libc::SOCK_SEQPACKET),
        (libc::SO_DOMAIN, libc::AF_UNIX),
        (libc::SO_ACCEPTCONN, 1),
    ] {
        let mut actual: libc::c_int = 0;
        let mut size = size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: actual and size are valid outputs for these integer options.
        unsafe {
            cvt(libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                option,
                (&mut actual as *mut libc::c_int).cast(),
                &mut size,
            ))?;
        }
        if actual != expected || size as usize != size_of::<libc::c_int>() {
            return Err(io::Error::other(
                "control descriptor must be a listening local sequential-packet socket",
            ));
        }
    }
    Ok(())
}

pub fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: fcntl operates on the supplied live descriptor without pointers.
    unsafe {
        let flags = cvt(libc::fcntl(fd, libc::F_GETFL))?;
        cvt(libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK))?;
        cvt(libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC))?;
    }
    Ok(())
}

pub fn accept(fd: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: null address arguments are permitted. Ownership of a new fd is unique.
    unsafe {
        let client = cvt(libc::accept4(
            fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
        ))?;
        Ok(OwnedFd::from_raw_fd(client))
    }
}

pub fn send_packet(fd: RawFd, bytes: &[u8]) -> io::Result<()> {
    // SAFETY: bytes describes valid readable storage; NOSIGNAL prevents client-triggered SIGPIPE.
    let result = unsafe { libc::send(fd, bytes.as_ptr().cast(), bytes.len(), libc::MSG_NOSIGNAL) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    if result as usize != bytes.len() {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "short local packet write",
        ));
    }
    Ok(())
}

pub fn receive_packet(fd: RawFd, bytes: &mut [u8], nonblocking: bool) -> io::Result<usize> {
    let flags = libc::MSG_TRUNC | if nonblocking { libc::MSG_DONTWAIT } else { 0 };
    // SAFETY: bytes is writable. MSG_TRUNC returns the *full* packet length, so
    // oversized messages are rejected instead of accepting their valid-looking prefix.
    let result = unsafe { libc::recv(fd, bytes.as_mut_ptr().cast(), bytes.len(), flags) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(result as usize)
}

pub struct Client {
    fd: OwnedFd,
}

impl Client {
    pub fn connect(path: &Path) -> io::Result<Self> {
        Self::connect_expected(path, 0)
    }

    #[cfg(test)]
    pub(crate) fn connect_for_test(path: &Path) -> io::Result<Self> {
        // Test listeners are unprivileged. Production always requires UID 0.
        Self::connect_expected(path, unsafe { libc::geteuid() })
    }

    fn connect_expected(path: &Path, expected_uid: u32) -> io::Result<Self> {
        use std::os::unix::ffi::OsStrExt;
        let bytes = path.as_os_str().as_bytes();
        // SAFETY: sockaddr_un is initialized and its path is checked before copying.
        unsafe {
            let mut address: libc::sockaddr_un = zeroed();
            if bytes.is_empty() || bytes.contains(&0) || bytes.len() >= address.sun_path.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid local socket path",
                ));
            }
            address.sun_family = libc::AF_UNIX as libc::sa_family_t;
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                address.sun_path.as_mut_ptr().cast(),
                bytes.len(),
            );
            let fd = OwnedFd::from_raw_fd(cvt(libc::socket(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                0,
            ))?);
            let timeout = libc::timeval {
                tv_sec: 3,
                tv_usec: 0,
            };
            for option in [libc::SO_RCVTIMEO, libc::SO_SNDTIMEO] {
                cvt(libc::setsockopt(
                    fd.as_raw_fd(),
                    libc::SOL_SOCKET,
                    option,
                    (&timeout as *const libc::timeval).cast(),
                    size_of::<libc::timeval>() as libc::socklen_t,
                ))?;
            }
            cvt(libc::connect(
                fd.as_raw_fd(),
                (&address as *const libc::sockaddr_un).cast(),
                size_of::<libc::sockaddr_un>() as libc::socklen_t,
            ))?;
            // The listening endpoint was created by the system manager, not a user process.
            authorize(fd.as_raw_fd(), expected_uid)?;
            Ok(Self { fd })
        }
    }

    pub fn command(&self, command: Command) -> io::Result<Response> {
        send_packet(self.fd.as_raw_fd(), &command.encode())?;
        let mut bytes = [0; RESPONSE_LEN];
        let size = receive_packet(self.fd.as_raw_fd(), &mut bytes, false)?;
        if size != RESPONSE_LEN {
            return Err(io::Error::other("invalid helper response size"));
        }
        Response::decode(&bytes)
    }
}

impl AsRawFd for Client {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::REQUEST_LEN;

    #[test]
    #[ignore = "requires host AF_UNIX access; run scripts/check-host.sh"]
    fn real_socket_credentials_are_enforced() {
        let (left, right) = socket_pair().unwrap();
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        authorize(left.as_raw_fd(), uid).unwrap();
        assert!(authorize(left.as_raw_fd(), uid.wrapping_add(1)).is_err());
        assert!(validate_listener(left.as_raw_fd()).is_err());
        drop(right);
    }

    #[test]
    #[ignore = "requires host AF_UNIX access; run scripts/check-host.sh"]
    fn real_packets_detect_truncation_without_losing_the_next_message() {
        let (left, right) = socket_pair().unwrap();
        let mut buffer = [0; REQUEST_LEN];
        let mut oversized = Command::Arm.encode().to_vec();
        oversized.extend_from_slice(&[0; 100]);
        send_packet(left.as_raw_fd(), &oversized).unwrap();
        assert_eq!(
            receive_packet(right.as_raw_fd(), &mut buffer, false).unwrap(),
            oversized.len()
        );
        send_packet(left.as_raw_fd(), &Command::Status.encode()).unwrap();
        assert_eq!(
            receive_packet(right.as_raw_fd(), &mut buffer, false).unwrap(),
            REQUEST_LEN
        );
        assert_eq!(Command::decode(&buffer).unwrap(), Command::Status);
    }
}
