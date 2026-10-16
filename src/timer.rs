//! A countdown the user starts from the island's timer page. Times are milliseconds of a
//! monotonic clock the caller passes in (GetTickCount64), so a change of the system time
//! does not move it.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum State {
    Ready, // not started: shows the minutes picked
    Running { end: u64 },
    Paused { left: u64 },
}

/// Minutes on offer on the timer page.
pub const PRESETS: [u32; 6] = [1, 5, 10, 15, 25, 45];

pub struct Timer {
    pub minutes: u32,
    pub state: State,
    pub done: bool, // it ran out, and the user has not touched it since
}

impl Timer {
    pub fn new() -> Self {
        Self { minutes: 25, state: State::Ready, done: false }
    }

    /// Milliseconds left at `now`.
    pub fn left(&self, now: u64) -> u64 {
        match self.state {
            State::Ready => self.minutes as u64 * 60_000,
            State::Running { end } => end.saturating_sub(now),
            State::Paused { left } => left,
        }
    }

    /// Running or paused: the closed island shows it.
    pub fn active(&self) -> bool {
        self.state != State::Ready
    }

    /// Starts, pauses or goes on.
    pub fn toggle(&mut self, now: u64) {
        self.done = false;
        self.state = match self.state {
            State::Ready => State::Running { end: now + self.left(now) },
            State::Running { end } => State::Paused { left: end.saturating_sub(now) },
            State::Paused { left } => State::Running { end: now + left },
        };
    }

    pub fn reset(&mut self) {
        self.done = false;
        self.state = State::Ready;
    }

    /// Picks the minutes, back to ready.
    pub fn set_minutes(&mut self, minutes: u32) {
        self.minutes = minutes.clamp(1, 180);
        self.reset();
    }

    /// A minute more or less, while it is not counting.
    pub fn nudge(&mut self, by: i32) {
        if self.state == State::Ready {
            self.set_minutes(self.minutes.saturating_add_signed(by));
        }
    }

    /// Whether it has run out at `now`; it is then ready again, with `done` set.
    pub fn finished(&mut self, now: u64) -> bool {
        if matches!(self.state, State::Running { end } if now >= end) {
            self.state = State::Ready;
            self.done = true;
            return true;
        }
        false
    }
}

/// "25:00", "0:42" is "00:42", an hour or more "1:05:00". Rounded up: a timer shows "00:01"
/// until it is over.
pub fn clock(ms: u64) -> String {
    let s = ms.div_ceil(1000);
    let (h, m, s) = (s / 3600, s % 3600 / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_down_pauses_and_rings() {
        let mut t = Timer::new();
        t.set_minutes(1);
        assert_eq!(clock(t.left(0)), "01:00");
        t.toggle(1_000); // start
        assert!(t.active());
        assert_eq!(clock(t.left(1_500)), "01:00"); // 59.5 s left
        assert_eq!(clock(t.left(2_000)), "00:59");
        t.toggle(31_000); // pause with 30 s left
        assert_eq!(t.left(99_000), 30_000);
        t.nudge(5); // not while it counts
        assert_eq!(t.minutes, 1);
        t.toggle(100_000); // go on: ends at 130 s
        assert!(!t.finished(129_999));
        assert!(t.finished(130_000));
        assert!(!t.active() && t.done);
        assert_eq!(t.left(0), 60_000); // ready with the same minutes
        t.nudge(-5);
        assert_eq!(t.minutes, 1);
        assert!(!t.done);
        t.nudge(2);
        assert_eq!(t.minutes, 3);
        assert_eq!(clock(65 * 60_000), "1:05:00");
        assert_eq!(clock(1), "00:01");
        assert_eq!(clock(0), "00:00");
    }
}
