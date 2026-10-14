use crate::renderer::cache::{MaterialId, MeshId};
use crate::renderer::types::InstanceRaw;

/// Батч отрисовки: один меш + один материал + список инстансов.
#[derive(Clone, Debug, PartialEq)]
pub struct DrawBatch {
    pub mesh: MeshId,
    pub material: MaterialId,
    pub instances: Vec<InstanceRaw>,
}

impl DrawBatch {
    pub fn new(mesh: MeshId, material: MaterialId) -> Self {
        Self {
            mesh,
            material,
            instances: Vec::new(),
        }
    }

    pub fn with_instance(mut self, instance: InstanceRaw) -> Self {
        self.instances.push(instance);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }

    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }
}

/// Кадровый список отрисовки: батчи сгруппированы по (меш, материал).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DrawList {
    pub batches: Vec<DrawBatch>,
}

impl DrawList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.batches.iter().all(|batch| batch.is_empty())
    }

    pub fn instance_count(&self) -> usize {
        self.batches.iter().map(|batch| batch.instances.len()).sum()
    }

    /// Добавляет инстанс к батчу с тем же (меш, материал) или создаёт новый.
    pub fn push(&mut self, mesh: MeshId, material: MaterialId, instance: InstanceRaw) {
        if let Some(batch) = self
            .batches
            .iter_mut()
            .find(|batch| batch.mesh == mesh && batch.material == material)
        {
            batch.instances.push(instance);
        } else {
            self.batches
                .push(DrawBatch::new(mesh, material).with_instance(instance));
        }
    }

    /// Собирает DrawList из пар (ключ меша, ключ материала) — плоский помощник.
    pub fn from_objects<'a>(
        mesh_of: impl Fn(&'a str) -> MeshId,
        material_of: impl Fn(&'a str) -> MaterialId,
        objects: impl IntoIterator<Item = (&'a str, &'a str, InstanceRaw)>,
    ) -> Self {
        let mut list = Self::new();
        for (mesh_key, material_key, instance) in objects {
            list.push(mesh_of(mesh_key), material_of(material_key), instance);
        }
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Mat4;

    fn instance(x: f32) -> InstanceRaw {
        InstanceRaw {
            model: Mat4::from_translation(glam::Vec3::new(x, 0.0, 0.0)).to_cols_array(),
            color: [1.0; 4],
        }
    }

    #[test]
    fn push_groups_same_mesh_material() {
        let mut list = DrawList::new();
        list.push(1, 1, instance(0.0));
        list.push(1, 1, instance(1.0));
        list.push(1, 2, instance(2.0));
        assert_eq!(list.batches.len(), 2);
        assert_eq!(list.batches[0].instance_count(), 2);
        assert_eq!(list.batches[1].instance_count(), 1);
        assert_eq!(list.instance_count(), 3);
        assert!(!list.is_empty());
    }

    #[test]
    fn from_objects_maps_keys() {
        let list = DrawList::from_objects(
            |key| if key == "a" { 1 } else { 2 },
            |_| 7,
            [("a", "m", instance(0.0)), ("b", "m", instance(1.0))],
        );
        assert_eq!(list.batches.len(), 2);
        assert_eq!(list.instance_count(), 2);
    }

    #[test]
    fn empty_list_reports_empty() {
        let list = DrawList::new();
        assert!(list.is_empty());
        assert_eq!(list.instance_count(), 0);
    }
}
