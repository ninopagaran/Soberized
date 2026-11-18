//! Status reporting to the current user service manager only.
use std::io;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::time::Duration;

pub fn status(message: &str) -> io::Result<()> {
    let Some(path) = std::env::var_os("NOTIFY_SOCKET") else {
        return Ok(());
    };
    let bytes = path.as_bytes();
    let address = if bytes.first() == Some(&b'@') {
        SocketAddr::from_abstract_name(&bytes[1..])?
    } else {
        SocketAddr::from_pathname(path)?
    };
    let socket = UnixDatagram::unbound()?;
    socket.set_write_timeout(Some(Duration::from_secs(1)))?;
    send(&socket, &address, message)
}

fn send(socket: &UnixDatagram, address: &SocketAddr, message: &str) -> io::Result<()> {
    if message.len() > 1024 || message.bytes().any(|b| matches!(b, b'\n' | b'\r' | 0)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid service status",
        ));
    }
    let data = format!("STATUS={message}");
    socket.connect_addr(address)?;
    if socket.send(data.as_bytes())? != data.len() {
        return Err(io::Error::other("short service notification"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires host AF_UNIX access; run scripts/check-host.sh"]
    fn status_is_sent_without_allowing_notification_field_injection() {
        let name = format!("soberized-notify-test-{}", std::process::id());
        let address = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let listener = UnixDatagram::bind_addr(&address).unwrap();
        listener
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let sender = UnixDatagram::unbound().unwrap();
        send(&sender, &address, "Input active; watching for disconnects").unwrap();
        let mut bytes = [0u8; 128];
        let size = listener.recv(&mut bytes).unwrap();
        assert_eq!(
            &bytes[..size],
            b"STATUS=Input active; watching for disconnects"
        );
        for bad in ["good\nREADY=1", "good\rMAINPID=1", "bad\0status"] {
            assert!(send(&sender, &address, bad).is_err());
        }
    }
}
