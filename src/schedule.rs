//! The helper owns timing. Clients cannot choose duration, amplitude, or interval.
use crate::protocol::{Command, ReplyCode, Response, State};

pub const INTERVAL_MS: u64 = 600_000;
pub const PULSE_MS: u64 = 250;
pub const LEASE_MS: u64 = 30_000;
pub const STARTUP_MS: u64 = 5_000;
pub const MAX_TICK_GAP_MS: u64 = 5_000;
pub const AMPLITUDE: i32 = 8_192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    None,
    Pulse(i32),
    Neutral,
    Exit,
}

pub struct Schedule {
    owner: Option<u64>,
    startup_deadline: u64,
    lease_deadline: u64,
    next_pulse: u64,
    pulse_end: Option<u64>,
    pulses: u32,
    last_tick: u64,
    stopped: bool,
}

impl Schedule {
    pub fn new(now: u64) -> Self {
        Self {
            owner: None,
            startup_deadline: now.saturating_add(STARTUP_MS),
            lease_deadline: 0,
            next_pulse: 0,
            pulse_end: None,
            pulses: 0,
            last_tick: now,
            stopped: false,
        }
    }

    pub fn command(&mut self, client: u64, command: Command, now: u64) -> Response {
        let code = match command {
            Command::Arm if self.owner.is_some() && self.owner != Some(client) => ReplyCode::Busy,
            Command::Arm if self.stopped => ReplyCode::NotOwner,
            Command::Arm => {
                if self.owner.is_none() {
                    self.owner = Some(client);
                    self.next_pulse = now.saturating_add(INTERVAL_MS);
                }
                self.lease_deadline = now.saturating_add(LEASE_MS);
                ReplyCode::Ok
            }
            Command::Renew if self.owner == Some(client) && !self.stopped => {
                self.lease_deadline = now.saturating_add(LEASE_MS);
                ReplyCode::Ok
            }
            Command::Renew => ReplyCode::NotOwner,
            Command::Stop => {
                self.stopped = true;
                ReplyCode::Ok
            }
            Command::Status => ReplyCode::Ok,
        };
        self.response(code, now)
    }

    pub fn disconnect(&mut self, client: u64) {
        if self.owner == Some(client) {
            self.stopped = true;
        }
    }

    pub fn tick(&mut self, now: u64) -> Effect {
        let previous_tick = self.last_tick;
        let gap = now.saturating_sub(self.last_tick);
        self.last_tick = now;
        if self.stopped
            || now < previous_tick
            || gap > MAX_TICK_GAP_MS
            || (self.owner.is_some() && now >= self.lease_deadline)
            || (self.owner.is_none() && now >= self.startup_deadline)
        {
            self.stopped = true;
            return Effect::Exit;
        }
        if let Some(end) = self.pulse_end {
            if now >= end {
                self.pulse_end = None;
                return Effect::Neutral;
            }
        } else if self.owner.is_some() && now >= self.next_pulse {
            self.pulses = self.pulses.saturating_add(1);
            self.pulse_end = Some(now.saturating_add(PULSE_MS));
            self.next_pulse = now.saturating_add(INTERVAL_MS);
            let sign = if self.pulses % 2 == 1 { 1 } else { -1 };
            return Effect::Pulse(AMPLITUDE * sign);
        }
        Effect::None
    }

    pub fn response(&self, code: ReplyCode, now: u64) -> Response {
        Response {
            state: if self.owner.is_none() || self.stopped {
                State::Ready
            } else if self.pulse_end.is_some() {
                State::Pulsing
            } else {
                State::Armed
            },
            code,
            next_seconds: if self.owner.is_some() && !self.stopped {
                self.next_pulse
                    .saturating_sub(now)
                    .div_ceil(1_000)
                    .min(u64::from(u32::MAX)) as u32
            } else {
                0
            },
            pulses: self.pulses,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advance(schedule: &mut Schedule, start: u64, end: u64) -> Effect {
        let mut effect = Effect::None;
        for now in (start..=end).step_by(250) {
            if now % 10_000 == 0 {
                schedule.command(1, Command::Renew, now);
            }
            effect = schedule.tick(now);
            if effect != Effect::None {
                break;
            }
        }
        effect
    }

    #[test]
    fn bounded_pulses_center_and_alternate() {
        let mut s = Schedule::new(0);
        assert_eq!(s.command(1, Command::Arm, 0).next_seconds, 600);
        assert_eq!(advance(&mut s, 0, INTERVAL_MS), Effect::Pulse(AMPLITUDE));
        assert_eq!(s.tick(INTERVAL_MS + PULSE_MS - 1), Effect::None);
        assert_eq!(s.tick(INTERVAL_MS + PULSE_MS), Effect::Neutral);
        assert_eq!(
            advance(&mut s, INTERVAL_MS + 500, INTERVAL_MS * 2),
            Effect::Pulse(-AMPLITUDE)
        );
    }

    #[test]
    fn lease_loss_suspend_and_disconnect_fail_closed() {
        for mode in 0..3 {
            let mut s = Schedule::new(0);
            s.command(1, Command::Arm, 0);
            if mode == 0 {
                for now in (0..LEASE_MS).step_by(1_000) {
                    assert_eq!(s.tick(now), Effect::None);
                }
                assert_eq!(s.tick(LEASE_MS), Effect::Exit);
            } else if mode == 1 {
                assert_eq!(s.tick(MAX_TICK_GAP_MS + 1), Effect::Exit);
            } else {
                s.disconnect(1);
                assert_eq!(s.tick(1), Effect::Exit);
            }
            assert_eq!(s.tick(1_000_000), Effect::Exit);
            assert_eq!(
                s.command(1, Command::Arm, 1_000_000).code,
                ReplyCode::NotOwner
            );
        }
    }

    #[test]
    fn owner_cannot_be_replaced_or_interval_shortened() {
        let mut s = Schedule::new(0);
        s.command(1, Command::Arm, 0);
        assert_eq!(s.command(2, Command::Arm, 1).code, ReplyCode::Busy);
        assert_eq!(s.command(2, Command::Renew, 1).code, ReplyCode::NotOwner);
        s.disconnect(2);
        assert_eq!(s.tick(1), Effect::None);
        assert_eq!(s.command(1, Command::Arm, 1_000).next_seconds, 599);
        s.command(2, Command::Stop, 1_000);
        assert_eq!(s.tick(1_000), Effect::Exit);
    }

    #[test]
    fn probing_never_keeps_an_unarmed_device_alive() {
        let mut s = Schedule::new(0);
        for now in (0..STARTUP_MS).step_by(1_000) {
            s.command(1, Command::Status, now);
            assert_eq!(s.tick(now), Effect::None);
        }
        assert_eq!(s.tick(STARTUP_MS), Effect::Exit);
    }

    #[test]
    fn clock_regression_stops_instead_of_sending_input() {
        let mut s = Schedule::new(1_000);
        s.command(1, Command::Arm, 1_000);
        assert_eq!(s.tick(1_001), Effect::None);
        assert_eq!(s.tick(1_000), Effect::Exit);
    }

    #[test]
    fn status_from_another_client_cannot_renew_the_owner_lease() {
        let mut s = Schedule::new(0);
        s.command(1, Command::Arm, 0);
        for now in (0..LEASE_MS).step_by(1_000) {
            s.command(2, Command::Status, now);
            assert_eq!(s.command(2, Command::Renew, now).code, ReplyCode::NotOwner);
            assert_eq!(s.tick(now), Effect::None);
        }
        assert_eq!(s.tick(LEASE_MS), Effect::Exit);
    }
}
