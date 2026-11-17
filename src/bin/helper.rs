use std::io;

fn main() {
    if let Err(error) = run() {
        eprintln!("soberized-helper: {error}");
        std::process::exit(1);
    }
}

fn run() -> io::Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments == ["--self-test"] {
        return soberized::sandbox::self_test();
    }
    if arguments == ["--self-test-ipc"] {
        return soberized::sandbox::ipc_self_test();
    }
    if arguments.len() != 2 || arguments[0] != "--owner-uid" {
        return Err(io::Error::other(
            "use systemd activation; expected --owner-uid UID",
        ));
    }
    let owner_uid = arguments[1]
        .parse::<u32>()
        .map_err(|_| io::Error::other("invalid owner UID"))?;
    soberized::helper::run(owner_uid)
}
