//! The helper owns timing. Clients cannot choose duration, amplitude, or interval.
use crate::protocol::{Command, ReplyCode, Response, State};

pub const INTERVAL_MS: u64 = 600_000;
pub const PULSE_MS: u64 = 250;
pub const LEASE_MS: u64 = 30_000;
pub const STARTUP_MS: u64 = 5_000;
pub const MAX_TICK_GAP_MS: u64 = 5_000;
pub const AMPLITUDE: i32 = 16_384;
pub const TEST_DELAY_MS: u64 = 10_000;
pub const TEST_COOLDOWN_MS: u64 = 60_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PulseKind {
    Scheduled,
    Test,
}

impl PulseKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Test => "test",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    Requested,
    OwnerDisconnected,
    ClockRegression,
    SchedulingGap,
    LeaseExpired,
    NoOwner,
}

impl StopReason {
    pub fn label(self) -> &'static str {
        match self {
            Self::Requested => "stop-requested",
            Self::OwnerDisconnected => "owner-disconnected",
            Self::ClockRegression => "clock-regressed",
            Self::SchedulingGap => "suspend-or-scheduling-gap",
            Self::LeaseExpired => "lease-expired",
            Self::NoOwner => "no-owner",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    None,
    Pulse(i32, PulseKind),
    Neutral,
    Exit(StopReason),
}

pub struct Schedule {
    owner: Option<u64>,
    startup_deadline: u64,
    lease_deadline: u64,
    next_pulse: u64,
    pulse_end: Option<u64>,
    pulses: u32,
    last_tick: u64,
    stop_reason: Option<StopReason>,
    test_due: Option<u64>,
    last_test: Option<u64>,
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
            stop_reason: None,
            test_due: None,
            last_test: None,
        }
    }

    pub fn command(&mut self, client: u64, command: Command, now: u64) -> Response {
        let code = match command {
            Command::Arm if self.owner.is_some() && self.owner != Some(client) => ReplyCode::Busy,
            Command::Arm if self.stop_reason.is_some() => ReplyCode::NotOwner,
            Command::Arm => {
                if self.owner.is_none() {
                    self.owner = Some(client);
                    self.next_pulse = now.saturating_add(INTERVAL_MS);
                }
                self.lease_deadline = now.saturating_add(LEASE_MS);
                ReplyCode::Ok
            }
            Command::Renew if self.owner == Some(client) && self.stop_reason.is_none() => {
                self.lease_deadline = now.saturating_add(LEASE_MS);
                ReplyCode::Ok
            }
            Command::Renew => ReplyCode::NotOwner,
            Command::Stop => {
                self.stop_reason.get_or_insert(StopReason::Requested);
                ReplyCode::Ok
            }
            Command::Status => ReplyCode::Ok,
            Command::TestInput if self.owner.is_none() || self.stop_reason.is_some() => {
                ReplyCode::NotOwner
            }
            Command::TestInput
                if self.pulse_end.is_some()
                    || self.test_due.is_some()
                    || self
                        .last_test
                        .is_some_and(|last| now.saturating_sub(last) < TEST_COOLDOWN_MS) =>
            {
                ReplyCode::Busy
            }
            Command::TestInput => {
                self.test_due = Some(now.saturating_add(TEST_DELAY_MS));
                self.last_test = Some(now);
                // A diagnostic replaces the next scheduled pulse, avoiding a
                // double movement if the ten-minute deadline is already near.
                self.next_pulse = now
                    .saturating_add(TEST_DELAY_MS)
                    .saturating_add(INTERVAL_MS);
                ReplyCode::Ok
            }
        };
        self.response(code, now)
    }

    pub fn disconnect(&mut self, client: u64) {
        if self.owner == Some(client) {
            self.stop_reason
                .get_or_insert(StopReason::OwnerDisconnected);
        }
    }

    pub fn tick(&mut self, now: u64) -> Effect {
        let previous_tick = self.last_tick;
        let gap = now.saturating_sub(self.last_tick);
        self.last_tick = now;
        if self.stop_reason.is_none() {
            self.stop_reason = if now < previous_tick {
                Some(StopReason::ClockRegression)
            } else if gap > MAX_TICK_GAP_MS {
                Some(StopReason::SchedulingGap)
            } else if self.owner.is_some() && now >= self.lease_deadline {
                Some(StopReason::LeaseExpired)
            } else if self.owner.is_none() && now >= self.startup_deadline {
                Some(StopReason::NoOwner)
            } else {
                None
            };
        }
        if let Some(reason) = self.stop_reason {
            return Effect::Exit(reason);
        }
        if let Some(end) = self.pulse_end {
            if now >= end {
                self.pulse_end = None;
                return Effect::Neutral;
            }
        } else if self.owner.is_some()
            && (self.test_due.is_some_and(|due| now >= due) || now >= self.next_pulse)
        {
            let kind = if self.test_due.is_some_and(|due| now >= due) {
                self.test_due = None;
                self.last_test = Some(now);
                PulseKind::Test
            } else {
                PulseKind::Scheduled
            };
            self.pulses = self.pulses.saturating_add(1);
            self.pulse_end = Some(now.saturating_add(PULSE_MS));
            self.next_pulse = now.saturating_add(INTERVAL_MS);
            let sign = if self.pulses % 2 == 1 { 1 } else { -1 };
            return Effect::Pulse(AMPLITUDE * sign, kind);
        }
        Effect::None
    }

    pub fn response(&self, code: ReplyCode, now: u64) -> Response {
        Response {
            state: if self.owner.is_none() || self.stop_reason.is_some() {
                State::Ready
            } else if self.pulse_end.is_some() {
                State::Pulsing
            } else {
                State::Armed
            },
            code,
            next_seconds: if self.owner.is_some() && self.stop_reason.is_none() {
                self.test_due
                    .unwrap_or(self.next_pulse)
                    .saturating_sub(now)
                    .div_ceil(1_000)
                    .min(u64::from(u32::MAX)) as u32
            } else {
                0
            },
            pulses: self.pulses,
        }
    }

    pub fn pulse_count(&self) -> u32 {
        self.pulses
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
        assert_eq!(
            advance(&mut s, 0, INTERVAL_MS),
            Effect::Pulse(AMPLITUDE, PulseKind::Scheduled)
        );
        assert_eq!(s.tick(INTERVAL_MS + PULSE_MS - 1), Effect::None);
        assert_eq!(s.tick(INTERVAL_MS + PULSE_MS), Effect::Neutral);
        assert_eq!(
            advance(&mut s, INTERVAL_MS + 500, INTERVAL_MS * 2),
            Effect::Pulse(-AMPLITUDE, PulseKind::Scheduled)
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
                assert_eq!(s.tick(LEASE_MS), Effect::Exit(StopReason::LeaseExpired));
            } else if mode == 1 {
                assert_eq!(
                    s.tick(MAX_TICK_GAP_MS + 1),
                    Effect::Exit(StopReason::SchedulingGap)
                );
            } else {
                s.disconnect(1);
                assert_eq!(s.tick(1), Effect::Exit(StopReason::OwnerDisconnected));
            }
            assert!(matches!(s.tick(1_000_000), Effect::Exit(_)));
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
        assert_eq!(s.tick(1_000), Effect::Exit(StopReason::Requested));
    }

    #[test]
    fn probing_never_keeps_an_unarmed_device_alive() {
        let mut s = Schedule::new(0);
        for now in (0..STARTUP_MS).step_by(1_000) {
            s.command(1, Command::Status, now);
            assert_eq!(s.tick(now), Effect::None);
        }
        assert_eq!(s.tick(STARTUP_MS), Effect::Exit(StopReason::NoOwner));
    }

    #[test]
    fn clock_regression_stops_instead_of_sending_input() {
        let mut s = Schedule::new(1_000);
        s.command(1, Command::Arm, 1_000);
        assert_eq!(s.tick(1_001), Effect::None);
        assert_eq!(s.tick(1_000), Effect::Exit(StopReason::ClockRegression));
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
        assert_eq!(s.tick(LEASE_MS), Effect::Exit(StopReason::LeaseExpired));
    }

    #[test]
    fn diagnostic_is_delayed_bounded_and_does_not_renew_the_lease() {
        let mut s = Schedule::new(0);
        assert_eq!(
            s.command(2, Command::TestInput, 0).code,
            ReplyCode::NotOwner
        );
        s.command(1, Command::Arm, 0);
        assert_eq!(s.command(2, Command::TestInput, 0).next_seconds, 10);
        assert_eq!(s.command(2, Command::TestInput, 1).code, ReplyCode::Busy);
        for now in (0..TEST_DELAY_MS).step_by(1_000) {
            assert_eq!(s.tick(now), Effect::None);
        }
        assert_eq!(
            s.tick(TEST_DELAY_MS),
            Effect::Pulse(AMPLITUDE, PulseKind::Test)
        );
        assert_eq!(s.tick(TEST_DELAY_MS + PULSE_MS), Effect::Neutral);
        assert_eq!(
            s.command(2, Command::TestInput, TEST_DELAY_MS + PULSE_MS)
                .code,
            ReplyCode::Busy
        );
        for now in (TEST_DELAY_MS + 1_000..LEASE_MS).step_by(1_000) {
            assert_eq!(s.tick(now), Effect::None);
        }
        assert_eq!(s.tick(LEASE_MS), Effect::Exit(StopReason::LeaseExpired));
    }

    #[test]
    fn owner_loss_cancels_queued_diagnostic_before_any_movement() {
        let mut s = Schedule::new(0);
        s.command(1, Command::Arm, 0);
        s.command(2, Command::TestInput, 0);
        s.disconnect(1);
        assert_eq!(s.tick(1), Effect::Exit(StopReason::OwnerDisconnected));
        assert_eq!(s.pulse_count(), 0);
    }

    #[test]
    fn diagnostic_cooldown_and_regular_cadence_are_enforced() {
        let mut s = Schedule::new(0);
        s.command(1, Command::Arm, 0);
        s.command(2, Command::TestInput, 0);
        assert_eq!(
            advance(&mut s, 0, TEST_DELAY_MS),
            Effect::Pulse(AMPLITUDE, PulseKind::Test)
        );
        assert_eq!(s.tick(TEST_DELAY_MS + PULSE_MS), Effect::Neutral);
        assert_eq!(
            s.response(ReplyCode::Ok, TEST_DELAY_MS + 1_000)
                .next_seconds,
            599
        );
        assert_eq!(
            advance(
                &mut s,
                TEST_DELAY_MS + 1_000,
                TEST_DELAY_MS + TEST_COOLDOWN_MS
            ),
            Effect::None
        );
        assert_eq!(
            s.command(2, Command::TestInput, TEST_DELAY_MS + TEST_COOLDOWN_MS)
                .code,
            ReplyCode::Ok
        );
    }
}
