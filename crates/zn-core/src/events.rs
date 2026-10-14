use std::collections::VecDeque;

use crate::ecs::World;

#[derive(Debug)]
pub struct EventQueue<E> {
    current: VecDeque<E>,
    previous: VecDeque<E>,
}

impl<E> Default for EventQueue<E> {
    fn default() -> Self {
        Self {
            current: VecDeque::new(),
            previous: VecDeque::new(),
        }
    }
}

impl<E> EventQueue<E> {
    pub fn send(&mut self, event: E) {
        self.current.push_back(event);
    }

    pub fn events(&self) -> impl Iterator<Item = &E> {
        self.current.iter()
    }

    pub fn len(&self) -> usize {
        self.current.len()
    }

    pub fn is_empty(&self) -> bool {
        self.current.is_empty()
    }

    pub fn drain(&mut self) -> impl Iterator<Item = E> + '_ {
        self.current.drain(..)
    }

    pub fn clear(&mut self) {
        self.current.clear();
    }

    pub fn update(&mut self) {
        std::mem::swap(&mut self.current, &mut self.previous);
        self.current.clear();
    }
}

pub fn send_event<E: 'static + Send + Sync>(world: &mut World, event: E) {
    world
        .get_resource_or_insert_with(EventQueue::default)
        .send(event);
}

pub fn take_events<E: 'static + Send + Sync>(world: &mut World) -> Vec<E> {
    world
        .get_resource_or_insert_with(EventQueue::default)
        .drain()
        .collect::<Vec<_>>()
}

pub fn event_count<E: 'static + Send + Sync>(world: &World) -> usize {
    world.get_resource::<EventQueue<E>>().map_or(0, |q| q.len())
}
