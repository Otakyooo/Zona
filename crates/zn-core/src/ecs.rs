use std::any::{Any, TypeId};
use std::collections::HashMap;

pub use hecs::{Component, DynamicBundle, Entity, NoSuchEntity};

#[derive(Default)]
pub struct Resources {
    map: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl Resources {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert<R: 'static + Send + Sync>(&mut self, resource: R) -> Option<R> {
        self.map
            .insert(TypeId::of::<R>(), Box::new(resource))
            .and_then(|boxed| boxed.downcast::<R>().ok())
            .map(|boxed| *boxed)
    }

    pub fn insert_default<R: 'static + Send + Sync + Default>(&mut self) -> bool {
        let inserted = !self.contains::<R>();
        self.entry::<R>().or_insert_with(|| Box::new(R::default()));
        inserted
    }

    pub fn get<R: 'static + Send + Sync>(&self) -> Option<&R> {
        self.map
            .get(&TypeId::of::<R>())
            .and_then(|b| b.downcast_ref::<R>())
    }

    pub fn get_mut<R: 'static + Send + Sync>(&mut self) -> Option<&mut R> {
        self.map
            .get_mut(&TypeId::of::<R>())
            .and_then(|b| b.downcast_mut::<R>())
    }

    pub fn get_or_insert_with<R: 'static + Send + Sync>(
        &mut self,
        f: impl FnOnce() -> R,
    ) -> &mut R {
        let entry = self.entry::<R>().or_insert_with(|| Box::new(f()));
        entry
            .downcast_mut::<R>()
            .expect("resource type id mismatch")
    }

    pub fn contains<R: 'static + Send + Sync>(&self) -> bool {
        self.map.contains_key(&TypeId::of::<R>())
    }

    pub fn remove<R: 'static + Send + Sync>(&mut self) -> Option<R> {
        self.map
            .remove(&TypeId::of::<R>())
            .and_then(|b| b.downcast::<R>().ok())
            .map(|b| *b)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    fn entry<R: 'static + Send + Sync>(
        &mut self,
    ) -> std::collections::hash_map::Entry<'_, TypeId, Box<dyn Any + Send + Sync>> {
        self.map.entry(TypeId::of::<R>())
    }
}

pub struct World {
    entities: hecs::World,
    resources: Resources,
    tick: u64,
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl World {
    pub fn new() -> Self {
        Self {
            entities: hecs::World::new(),
            resources: Resources::new(),
            tick: 0,
        }
    }

    pub fn spawn(&mut self, components: impl DynamicBundle) -> Entity {
        self.entities.spawn(components)
    }

    pub fn despawn(&mut self, entity: Entity) -> bool {
        self.entities.despawn(entity).is_ok()
    }

    pub fn clear(&mut self) {
        self.entities.clear();
        self.resources = Resources::new();
        self.tick = 0;
    }

    pub fn insert_on<T: Component>(
        &mut self,
        entity: Entity,
        component: T,
    ) -> Result<(), NoSuchEntity> {
        self.entities
            .insert_one(entity, component)
            .map_err(|_| NoSuchEntity)
    }

    pub fn remove<T: Component>(&mut self, entity: Entity) -> Option<T> {
        self.entities
            .remove::<(T,)>(entity)
            .ok()
            .map(|(component,)| component)
    }

    pub fn get<T: Component>(&self, entity: Entity) -> Option<hecs::Ref<'_, T>> {
        self.entities.get::<&T>(entity).ok()
    }

    pub fn get_mut<T: Component>(&mut self, entity: Entity) -> Option<hecs::RefMut<'_, T>> {
        self.entities.get::<&mut T>(entity).ok()
    }

    pub fn query<'w, Q: hecs::Query>(&'w mut self) -> hecs::QueryBorrow<'w, Q> {
        self.entities.query::<Q>()
    }

    pub fn entities(&self) -> &hecs::World {
        &self.entities
    }

    pub fn entities_mut(&mut self) -> &mut hecs::World {
        &mut self.entities
    }

    pub fn insert_resource<R: 'static + Send + Sync>(&mut self, resource: R) -> Option<R> {
        self.resources.insert(resource)
    }

    pub fn insert_resource_default<R: 'static + Send + Sync + Default>(&mut self) -> bool {
        self.resources.insert_default::<R>()
    }

    pub fn get_resource<R: 'static + Send + Sync>(&self) -> Option<&R> {
        self.resources.get::<R>()
    }

    pub fn get_resource_mut<R: 'static + Send + Sync>(&mut self) -> Option<&mut R> {
        self.resources.get_mut::<R>()
    }

    pub fn get_resource_or_insert_with<R: 'static + Send + Sync>(
        &mut self,
        f: impl FnOnce() -> R,
    ) -> &mut R {
        self.resources.get_or_insert_with(f)
    }

    pub fn contains_resource<R: 'static + Send + Sync>(&self) -> bool {
        self.resources.contains::<R>()
    }

    pub fn remove_resource<R: 'static + Send + Sync>(&mut self) -> Option<R> {
        self.resources.remove::<R>()
    }

    pub fn resources(&self) -> &Resources {
        &self.resources
    }

    pub fn resources_mut(&mut self) -> &mut Resources {
        &mut self.resources
    }

    pub fn tick(&self) -> u64 {
        self.tick
    }

    pub fn advance_tick(&mut self) {
        self.tick += 1;
    }
}
