//! Frame pacing for the interactive loop. It owns no clock: callers pass the
//! time in, so the rules can be tested without waiting.

use std::time::{Duration, Instant};

/// How long a loop waits between steps while the plate is at rest. Sound is
/// noticed within one tick, and the capture ring buffer holds a full second,
/// so no audio is lost in between. A key press still wakes the loop at once.
pub const IDLE_TICK: Duration = Duration::from_millis(50);

/// The longest time step the engine is given after a rest or a stall.
const MAX_TICKS: u32 = 4;

#[derive(Debug, Clone)]
pub struct Pacer {
    frame_time: Duration,
    deadline: Instant,
    previous: Instant,
    resting: bool,
}

impl Pacer {
    pub fn new(frame_time: Duration, now: Instant) -> Self {
        Self {
            frame_time,
            deadline: now,
            previous: now,
            resting: false,
        }
    }

    /// The time step for the frame starting at `now`: the real time since the
    /// previous one, which keeps smoothing constants right when frames run
    /// late. A quarter frame is the least and a few ticks the most, so one
    /// stall (a suspended laptop, a slow terminal) cannot jolt the simulation.
    pub fn begin(&mut self, now: Instant) -> f32 {
        let tick = if self.resting {
            IDLE_TICK
        } else {
            self.frame_time
        };
        let dt = now
            .saturating_duration_since(self.previous)
            .clamp(self.frame_time / 4, tick * MAX_TICKS);
        self.previous = now;
        dt.as_secs_f32()
    }

    /// Plan the wait after a step. `resting` says that the step drew nothing,
    /// so that the next one can come later. Returns the deadline.
    pub fn schedule(&mut self, now: Instant, resting: bool) -> Instant {
        self.resting = resting;
        let tick = if resting { IDLE_TICK } else { self.frame_time };
        self.deadline += tick;
        // After falling more than a tick behind, start over rather than
        // racing through the frames that were missed.
        if now > self.deadline + tick {
            self.deadline = now;
        }
        self.deadline
    }

    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Something needs showing now: don't wait out the rest of the tick.
    pub fn expedite(&mut self, now: Instant) {
        self.deadline = now;
    }

    /// Whether the last wait was a rest tick.
    pub fn is_resting(&self) -> bool {
        self.resting
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Duration = Duration::from_micros(16_667);

    fn ms(milliseconds: u64) -> Duration {
        Duration::from_millis(milliseconds)
    }

    #[test]
    fn frames_are_one_frame_time_apart_and_the_step_is_the_real_time() {
        let start = Instant::now();
        let mut pacer = Pacer::new(FRAME, start);
        let first = pacer.begin(start + ms(17));
        assert!((first - 0.017).abs() < 1e-6);
        let due = pacer.schedule(start + ms(18), false);
        assert_eq!(due, start + FRAME);
        let second = pacer.begin(start + ms(34));
        assert!((second - 0.017).abs() < 1e-6);
        assert_eq!(pacer.schedule(start + ms(35), false), start + FRAME * 2);
        assert!(!pacer.is_resting());
    }

    #[test]
    fn one_stall_cannot_jolt_the_simulation() {
        let start = Instant::now();
        let mut pacer = Pacer::new(FRAME, start);
        // A stall of two seconds is cut to four frames, and zero to a quarter.
        let stalled = pacer.begin(start + ms(2_000));
        assert!((stalled - 4.0 * FRAME.as_secs_f32()).abs() < 1e-6);
        let at_once = pacer.begin(start + ms(2_000));
        assert!((at_once - FRAME.as_secs_f32() / 4.0).abs() < 1e-6);
        // Instants that go backwards are not an error.
        let _ = pacer.begin(start);
    }

    #[test]
    fn a_resting_plate_waits_one_tick_and_steps_by_the_time_that_passed() {
        let start = Instant::now();
        let mut pacer = Pacer::new(FRAME, start);
        let mut now = start;
        for expected in 1..=4 {
            let due = pacer.schedule(now, true);
            assert_eq!(due, start + IDLE_TICK * expected);
            assert!(pacer.is_resting());
            now = due;
            // The step after a rest tick is the tick itself, not one frame.
            let dt = pacer.begin(now);
            assert!((dt - IDLE_TICK.as_secs_f32()).abs() < 1e-6, "{dt}");
        }
        // After a long suspend the step is bounded by four ticks.
        let dt = pacer.begin(now + Duration::from_secs(60));
        assert!((dt - 4.0 * IDLE_TICK.as_secs_f32()).abs() < 1e-6);
        // Back to drawing: frames again.
        let due = pacer.schedule(now + Duration::from_secs(60), false);
        assert!(due >= now + Duration::from_secs(60));
        assert!(!pacer.is_resting());
    }

    #[test]
    fn a_key_during_a_rest_brings_the_next_step_forward() {
        let start = Instant::now();
        let mut pacer = Pacer::new(FRAME, start);
        let due = pacer.schedule(start, true);
        assert_eq!(due, start + IDLE_TICK);
        pacer.expedite(start + ms(7));
        assert_eq!(pacer.deadline(), start + ms(7));
        // The step that follows is one frame later at the earliest.
        assert_eq!(pacer.schedule(start + ms(8), false), start + ms(7) + FRAME);
    }

    #[test]
    fn falling_behind_resynchronizes_instead_of_catching_up() {
        let start = Instant::now();
        let mut pacer = Pacer::new(FRAME, start);
        let late = start + ms(500);
        assert_eq!(pacer.schedule(late, false), late);
        assert_eq!(pacer.schedule(late + ms(1), false), late + FRAME);
        // A high frame rate has the same rule.
        let mut fast = Pacer::new(Duration::from_micros(4_167), start);
        assert_eq!(fast.schedule(start + ms(101), true), start + ms(101));
        // Within two ticks it keeps its schedule.
        let mut steady = Pacer::new(Duration::from_micros(4_167), start);
        assert_eq!(steady.schedule(start + ms(99), true), start + IDLE_TICK);
    }
}
