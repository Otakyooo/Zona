use std::time::Duration;

pub const SIMULATION_HZ: u32 = 60;
pub const SIMULATION_STEP: Duration = Duration::from_nanos(16_666_667);

pub fn duration_from_hz(hz: u32) -> Duration {
    Duration::from_secs_f64(1.0 / hz as f64)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Clock {
    pub tick: u64,
    pub frame: u64,
    pub simulation_time: Duration,
    pub frame_time: Duration,
}

impl Clock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn advance_frame(&mut self, frame_time: Duration) {
        self.frame += 1;
        self.frame_time = frame_time;
    }

    pub fn advance_tick(&mut self, dt: Duration) {
        self.tick += 1;
        self.simulation_time += dt;
    }

    pub fn simulation_seconds(&self) -> f64 {
        self.simulation_time.as_secs_f64()
    }
}

#[derive(Clone, Debug)]
pub struct FixedStep {
    fixed_dt: Duration,
    accumulator: Duration,
    max_steps_per_frame: u32,
}

impl FixedStep {
    pub fn new(fixed_dt: Duration) -> Self {
        Self {
            fixed_dt,
            accumulator: Duration::ZERO,
            max_steps_per_frame: 8,
        }
    }

    pub fn from_hz(hz: u32) -> Self {
        Self::new(duration_from_hz(hz))
    }

    pub fn fixed_dt(&self) -> Duration {
        self.fixed_dt
    }

    pub fn accumulator(&self) -> Duration {
        self.accumulator
    }

    pub fn max_steps_per_frame(&self) -> u32 {
        self.max_steps_per_frame
    }

    pub fn set_max_steps_per_frame(&mut self, max: u32) {
        self.max_steps_per_frame = max.max(1);
    }

    pub fn reset(&mut self) {
        self.accumulator = Duration::ZERO;
    }

    pub fn accumulate(&mut self, frame_delta: Duration) -> u32 {
        self.accumulator += frame_delta;
        let mut steps = 0;
        while self.accumulator >= self.fixed_dt && steps < self.max_steps_per_frame {
            self.accumulator -= self.fixed_dt;
            steps += 1;
        }
        if self.accumulator >= self.fixed_dt {
            log::debug!(
                target: "zn_core::time",
                "dropping {:?} of catch-up time (max {} steps)",
                self.accumulator,
                self.max_steps_per_frame
            );
            self.accumulator = Duration::ZERO;
        }
        steps
    }
}

impl Default for FixedStep {
    fn default() -> Self {
        Self::new(SIMULATION_STEP)
    }
}
