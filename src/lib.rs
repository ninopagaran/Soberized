#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
compile_error!("Soberized v1 supports Linux x86-64 only");

pub mod device;
pub mod helper;
pub mod ipc;
pub mod protocol;
pub mod sandbox;
pub mod schedule;
pub mod sober;
pub mod units;

pub const APP_ID: &str = "org.vinegarhq.Sober";
pub const SOCKET_PATH: &str = "/run/soberized/control.sock";
pub const USER_UNIT: &str = "soberized-session.service";
pub const HELPER_UNIT: &str = "soberized-helper.service";
pub const HELPER_PATH: &str = "/usr/lib/soberized/soberized-helper";
