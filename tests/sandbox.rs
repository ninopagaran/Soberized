use std::process::Command;

#[test]
fn installed_filter_rejects_dangerous_syscalls_in_a_disposable_process() {
    let output = Command::new(env!("CARGO_BIN_EXE_soberized-helper"))
        .arg("--self-test")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("PASS:"));
}

#[test]
fn helper_rejects_direct_execution_without_activation() {
    let output = Command::new(env!("CARGO_BIN_EXE_soberized-helper"))
        .args(["--owner-uid", "1000"])
        .env_remove("LISTEN_PID")
        .env_remove("LISTEN_FDS")
        .env_remove("LISTEN_FDNAMES")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("systemd activation") || error.contains("unprivileged"),
        "{error}"
    );
}

#[test]
#[ignore = "requires host AF_UNIX access; run scripts/check-host.sh"]
fn local_packets_work_under_the_installed_filter() {
    let output = Command::new(env!("CARGO_BIN_EXE_soberized-helper"))
        .arg("--self-test-ipc")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("PASS:"));
}
