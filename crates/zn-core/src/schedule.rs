use crate::ecs::World;

pub type SystemFn = Box<dyn FnMut(&mut World)>;

pub struct Schedule {
    systems: Vec<(String, SystemFn)>,
}

impl Default for Schedule {
    fn default() -> Self {
        Self::new()
    }
}

impl Schedule {
    pub fn new() -> Self {
        Self {
            systems: Vec::new(),
        }
    }

    pub fn add<S: Into<String>>(&mut self, name: S, system: impl FnMut(&mut World) + 'static) {
        self.systems.push((name.into(), Box::new(system)));
    }

    pub fn run(&mut self, world: &mut World) {
        for (name, system) in self.systems.iter_mut() {
            log::trace!(target: "zn_core::schedule", "running system {}", name);
            system(world);
        }
    }

    pub fn len(&self) -> usize {
        self.systems.len()
    }

    pub fn is_empty(&self) -> bool {
        self.systems.is_empty()
    }

    pub fn system_names(&self) -> impl Iterator<Item = &str> {
        self.systems.iter().map(|(name, _)| name.as_str())
    }
}
