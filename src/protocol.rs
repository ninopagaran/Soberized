//! Fixed-size, versioned SOCK_SEQPACKET messages. No text, paths, or input events.
use std::io;

pub const REQUEST_LEN: usize = 8;
pub const RESPONSE_LEN: usize = 16;
const MAGIC: [u8; 4] = [b'S', b'B', b'Z', 4];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Command {
    Arm = 1,
    Renew = 2,
    Stop = 3,
    Status = 4,
    TestInput = 5,
    Prepare = 6,
    UiSelect = 7,
}

impl Command {
    pub fn encode(self) -> [u8; REQUEST_LEN] {
        let mut bytes = [0; REQUEST_LEN];
        bytes[..4].copy_from_slice(&MAGIC);
        bytes[4] = self as u8;
        bytes
    }

    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() != REQUEST_LEN || bytes[..4] != MAGIC || bytes[5..] != [0; 3] {
            return Err(invalid());
        }
        match bytes[4] {
            1 => Ok(Self::Arm),
            2 => Ok(Self::Renew),
            3 => Ok(Self::Stop),
            4 => Ok(Self::Status),
            5 => Ok(Self::TestInput),
            6 => Ok(Self::Prepare),
            7 => Ok(Self::UiSelect),
            _ => Err(invalid()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    Ready = 0,
    Armed = 1,
    Pulsing = 2,
    Prepared = 3,
    Selecting = 4,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ReplyCode {
    Ok = 0,
    Busy = 1,
    NotOwner = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Response {
    pub state: State,
    pub code: ReplyCode,
    pub next_seconds: u32,
    pub pulses: u32,
}

impl Response {
    pub fn encode(self) -> [u8; RESPONSE_LEN] {
        let mut bytes = [0; RESPONSE_LEN];
        bytes[..4].copy_from_slice(&MAGIC);
        bytes[4] = self.state as u8;
        bytes[5] = self.code as u8;
        bytes[8..12].copy_from_slice(&self.next_seconds.to_be_bytes());
        bytes[12..16].copy_from_slice(&self.pulses.to_be_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() != RESPONSE_LEN || bytes[..4] != MAGIC || bytes[6..8] != [0; 2] {
            return Err(invalid());
        }
        let state = match bytes[4] {
            0 => State::Ready,
            1 => State::Armed,
            2 => State::Pulsing,
            3 => State::Prepared,
            4 => State::Selecting,
            _ => return Err(invalid()),
        };
        let code = match bytes[5] {
            0 => ReplyCode::Ok,
            1 => ReplyCode::Busy,
            2 => ReplyCode::NotOwner,
            _ => return Err(invalid()),
        };
        Ok(Self {
            state,
            code,
            next_seconds: u32::from_be_bytes(bytes[8..12].try_into().unwrap()),
            pulses: u32::from_be_bytes(bytes[12..16].try_into().unwrap()),
        })
    }
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid or incompatible Soberized protocol message; reinstall the current CLI and helper together",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_round_trip_and_reject_untrusted_bytes() {
        for command in [
            Command::Arm,
            Command::Renew,
            Command::Stop,
            Command::Status,
            Command::TestInput,
            Command::Prepare,
            Command::UiSelect,
        ] {
            assert_eq!(Command::decode(&command.encode()).unwrap(), command);
        }
        for length in 0..=32 {
            assert!(Command::decode(&vec![0; length]).is_err());
        }
        for index in 0..REQUEST_LEN {
            let mut bytes = Command::Arm.encode();
            bytes[index] = 255;
            assert!(Command::decode(&bytes).is_err());
        }
    }

    #[test]
    fn responses_reject_unknown_versions_and_fields() {
        let response = Response {
            state: State::Armed,
            code: ReplyCode::Ok,
            next_seconds: 600,
            pulses: u32::MAX,
        };
        assert_eq!(Response::decode(&response.encode()).unwrap(), response);
        let selecting = Response {
            state: State::Selecting,
            ..response
        };
        assert_eq!(Response::decode(&selecting.encode()).unwrap(), selecting);
        for index in 0..8 {
            let mut bytes = response.encode();
            bytes[index] = 255;
            assert!(Response::decode(&bytes).is_err());
        }
    }

    #[test]
    fn helpers_without_selection_diagnostics_are_not_accepted() {
        for version in [1, 2, 3] {
            let mut request = Command::Prepare.encode();
            request[3] = version;
            assert!(Command::decode(&request).is_err());
            let mut response = Response {
                state: State::Prepared,
                code: ReplyCode::Ok,
                next_seconds: 0,
                pulses: 0,
            }
            .encode();
            response[3] = version;
            assert!(Response::decode(&response).is_err());
        }
    }
}
