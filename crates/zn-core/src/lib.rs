pub mod ecs;
pub mod events;
pub mod schedule;
pub mod time;

pub use ecs::{Entity, Resources, World};
pub use events::{send_event, take_events, EventQueue};
pub use schedule::{Schedule, SystemFn};
pub use time::{Clock, FixedStep, SIMULATION_HZ, SIMULATION_STEP, duration_from_hz};

pub mod prelude {
    pub use crate::ecs::{Component, Entity, World};
    pub use crate::events::{send_event, take_events, EventQueue};
    pub use crate::schedule::Schedule;
    pub use crate::time::{Clock, FixedStep};
}
