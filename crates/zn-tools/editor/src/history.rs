//! История правок редактора (M4g‑G4a): undo/redo по объектам сцены.
//!
//! Модель — стопка команд; каждая команда — набор элементарных правок
//! (замена/добавление/удаление объекта, замена атмосферы). Откат применяет
//! «до» в обратном порядке, повтор — «после». Подряд идущие однотипные
//! замены тех же объектов в пределах `COALESCE_WINDOW` склеиваются:
//! перетаскивание DragValue или gizmo — одна команда истории, а не сотни
//! кадровых правок.

use std::time::{Duration, Instant};

use zn_scene::{Scene, SceneEnvironment, SceneObject};

/// Глубина истории (команд): старые вытесняются.
pub const HISTORY_LIMIT: usize = 200;

/// Окно склейки однотипных замен (драг поля/gizmo) в одну команду.
pub const COALESCE_WINDOW: Duration = Duration::from_millis(700);

/// Элементарная правка сцены — единица отката.
#[derive(Clone, Debug, PartialEq)]
pub enum SceneEdit {
    /// Замена объекта (индекс не меняется).
    Change {
        index: usize,
        before: Box<SceneObject>,
        after: Box<SceneObject>,
    },
    /// Добавление объекта на позицию `index`.
    Add {
        index: usize,
        object: Box<SceneObject>,
    },
    /// Удаление объекта с позиции `index`.
    Remove {
        index: usize,
        object: Box<SceneObject>,
    },
    /// Замена атмосферы сцены (G4): солнце, ambient, туман, небо.
    Environment {
        before: Box<SceneEnvironment>,
        after: Box<SceneEnvironment>,
    },
}

impl SceneEdit {
    /// Применяет «после» (повтор команды).
    fn apply(&self, scene: &mut Scene) {
        match self {
            Self::Change { index, after, .. } => match scene.objects.get_mut(*index) {
                Some(slot) => *slot = (**after).clone(),
                None => log::warn!("история: объект {index} исчез — повтор пропущен"),
            },
            Self::Add { index, object } => {
                let index = (*index).min(scene.objects.len());
                scene.objects.insert(index, (**object).clone());
            }
            Self::Remove { index, .. } => {
                if *index < scene.objects.len() {
                    scene.objects.remove(*index);
                } else {
                    log::warn!("история: объект {index} уже удалён — повтор пропущен");
                }
            }
            Self::Environment { after, .. } => scene.environment = (**after).clone(),
        }
    }

    /// Применяет «до» (откат команды).
    fn revert(&self, scene: &mut Scene) {
        match self {
            Self::Change { index, before, .. } => match scene.objects.get_mut(*index) {
                Some(slot) => *slot = (**before).clone(),
                None => log::warn!("история: объект {index} исчез — откат пропущен"),
            },
            Self::Add { index, .. } => {
                if *index < scene.objects.len() {
                    scene.objects.remove(*index);
                } else {
                    log::warn!("история: объект {index} уже удалён — откат пропущен");
                }
            }
            Self::Remove { index, object } => {
                let index = (*index).min(scene.objects.len());
                scene.objects.insert(index, (**object).clone());
            }
            Self::Environment { before, .. } => scene.environment = (**before).clone(),
        }
    }
}

/// Команда истории: имя для интерфейса и правки сцены.
#[derive(Clone, Debug, PartialEq)]
pub struct EditCommand {
    label: String,
    edits: Vec<SceneEdit>,
}

impl EditCommand {
    pub fn new(label: impl Into<String>, edits: Vec<SceneEdit>) -> Self {
        Self {
            label: label.into(),
            edits,
        }
    }

    /// Замена одного объекта — правка свойств или gizmo.
    pub fn change(
        label: impl Into<String>,
        index: usize,
        before: &SceneObject,
        after: &SceneObject,
    ) -> Self {
        Self::new(
            label,
            vec![SceneEdit::Change {
                index,
                before: Box::new(before.clone()),
                after: Box::new(after.clone()),
            }],
        )
    }

    /// Добавление объекта на позицию `index`.
    pub fn add(index: usize, object: &SceneObject) -> Self {
        Self::new(
            "размещение объекта",
            vec![SceneEdit::Add {
                index,
                object: Box::new(object.clone()),
            }],
        )
    }

    /// Удаление объекта с позиции `index`.
    pub fn remove(index: usize, object: &SceneObject) -> Self {
        Self::new(
            "удаление объекта",
            vec![SceneEdit::Remove {
                index,
                object: Box::new(object.clone()),
            }],
        )
    }

    /// Замена атмосферы сцены (G4): пресет или правка поля панели.
    pub fn environment(
        label: impl Into<String>,
        before: &SceneEnvironment,
        after: &SceneEnvironment,
    ) -> Self {
        Self::new(
            label,
            vec![SceneEdit::Environment {
                before: Box::new(before.clone()),
                after: Box::new(after.clone()),
            }],
        )
    }

    /// Имя команды для меню «Отменить»/«Вернуть».
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Правки команды.
    pub fn edits(&self) -> &[SceneEdit] {
        &self.edits
    }

    /// Пробует склеить замены: у текущей команды остаётся «до», у новой
    /// берётся «после». Возвращает false, если команды не одного вида.
    fn try_merge(&mut self, next: &EditCommand) -> bool {
        if self.label != next.label || self.edits.len() != next.edits.len() {
            return false;
        }
        let mut merged = Vec::with_capacity(self.edits.len());
        for (current, new) in self.edits.iter().zip(&next.edits) {
            match (current, new) {
                (
                    SceneEdit::Change {
                        index: left,
                        before,
                        ..
                    },
                    SceneEdit::Change {
                        index: right,
                        after,
                        ..
                    },
                ) if left == right => merged.push(SceneEdit::Change {
                    index: *left,
                    before: before.clone(),
                    after: after.clone(),
                }),
                (SceneEdit::Environment { before, .. }, SceneEdit::Environment { after, .. }) => {
                    merged.push(SceneEdit::Environment {
                        before: before.clone(),
                        after: after.clone(),
                    })
                }
                _ => return false,
            }
        }
        self.edits = merged;
        true
    }
}

/// Команда с временем постановки — для склейки серий правок.
#[derive(Clone, Debug)]
struct Entry {
    command: EditCommand,
    at: Instant,
}

/// Стопки undo/redo редактора.
#[derive(Clone, Debug)]
pub struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    limit: usize,
}

impl Default for History {
    fn default() -> Self {
        Self::new(HISTORY_LIMIT)
    }
}

impl History {
    /// История с ограничением глубины (`limit` команд, минимум одна).
    pub fn new(limit: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            limit: limit.max(1),
        }
    }

    /// Очищает историю (новая сцена, открытие файла).
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Имя команды, которую отменит `undo`.
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|entry| entry.command.label())
    }

    /// Имя команды, которую повторит `redo`.
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|entry| entry.command.label())
    }

    /// Число команд в стопке отката (для статуса и тестов).
    pub fn depth(&self) -> usize {
        self.undo.len()
    }

    /// Кладёт команду в историю. Идущие подряд однотипные замены тех же
    /// объектов в пределах `COALESCE_WINDOW` склеиваются в одну команду.
    pub fn push(&mut self, command: EditCommand, now: Instant) {
        self.redo.clear();
        if let Some(last) = self.undo.last_mut() {
            if now.saturating_duration_since(last.at) <= COALESCE_WINDOW
                && last.command.try_merge(&command)
            {
                last.at = now;
                return;
            }
        }
        self.undo.push(Entry { command, at: now });
        if self.undo.len() > self.limit {
            self.undo.remove(0);
        }
    }

    /// Откатывает последнюю команду; возвращает её имя.
    pub fn undo(&mut self, scene: &mut Scene, now: Instant) -> Option<String> {
        let entry = self.undo.pop()?;
        for edit in entry.command.edits().iter().rev() {
            edit.revert(scene);
        }
        let label = entry.command.label().to_string();
        self.redo.push(Entry {
            command: entry.command,
            at: now,
        });
        Some(label)
    }

    /// Повторяет последнюю отменённую команду; возвращает её имя.
    pub fn redo(&mut self, scene: &mut Scene, now: Instant) -> Option<String> {
        let entry = self.redo.pop()?;
        for edit in entry.command.edits() {
            edit.apply(scene);
        }
        let label = entry.command.label().to_string();
        self.undo.push(Entry {
            command: entry.command,
            at: now,
        });
        Some(label)
    }
}

/// Имя правки свойств по первому изменившемуся полю — для меню «Отменить».
pub fn object_edit_label(before: &SceneObject, after: &SceneObject) -> String {
    let label = if before.position != after.position {
        "позиция"
    } else if before.scale != after.scale {
        "масштаб"
    } else if before.yaw != after.yaw {
        "поворот"
    } else if before.color != after.color {
        "цвет"
    } else if before.asset != after.asset {
        "ассет"
    } else if before.material != after.material {
        "материал"
    } else if before.texture != after.texture {
        "текстура"
    } else if before.uv_scale != after.uv_scale || before.world_uv != after.world_uv {
        "текстура (UV)"
    } else if before.roughness != after.roughness {
        "шероховатость"
    } else if before.metallic != after.metallic {
        "металличность"
    } else if before.emissive != after.emissive {
        "свечение"
    } else if before.collider != after.collider || before.solid != after.solid {
        "коллайдер"
    } else if before.loot != after.loot {
        "лут"
    } else {
        "объект"
    };
    label.to_string()
}

/// Имя правки атмосферы по первому изменившемуся полю — для меню «Отменить».
pub fn environment_edit_label(before: &SceneEnvironment, after: &SceneEnvironment) -> String {
    let label = if before.sun_direction != after.sun_direction {
        "солнце (направление)"
    } else if before.sun_color != after.sun_color {
        "солнце (цвет)"
    } else if before.sun_intensity != after.sun_intensity {
        "солнце (яркость)"
    } else if before.ambient_color != after.ambient_color {
        "ambient (цвет)"
    } else if before.ambient_intensity != after.ambient_intensity {
        "ambient (яркость)"
    } else if before.fog_color != after.fog_color {
        "туман (цвет)"
    } else if before.fog_density != after.fog_density {
        "туман (плотность)"
    } else if before.sky_top != after.sky_top {
        "небо (зенит)"
    } else if before.sky_horizon != after.sky_horizon {
        "небо (горизонт)"
    } else {
        "атмосфера"
    };
    label.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Vec3, Vec4};

    fn scene_with_objects(count: usize) -> Scene {
        let mut scene = Scene::new("тест");
        for index in 0..count {
            scene.add(SceneObject::new(
                Vec3::new(index as f32, 0.0, 0.0),
                Vec3::ONE,
                Vec4::ONE,
            ));
        }
        scene
    }

    fn moved(object: &SceneObject, x: f32) -> SceneObject {
        let mut object = object.clone();
        object.position.x = x;
        object
    }

    #[test]
    fn change_undo_and_redo_restore_positions() {
        let mut scene = scene_with_objects(2);
        let before = scene.objects[0].clone();
        let after = moved(&before, 5.0);
        scene.objects[0] = after.clone();

        let mut history = History::default();
        let now = Instant::now();
        history.push(EditCommand::change("позиция", 0, &before, &after), now);

        assert_eq!(history.undo_label(), Some("позиция"));
        assert!(history.can_undo() && !history.can_redo());

        let label = history.undo(&mut scene, now).expect("откат");
        assert_eq!(label, "позиция");
        assert_eq!(scene.objects[0].position.x, 0.0, "«до» восстановлено");
        assert_eq!(scene.objects[1].position.x, 1.0, "сосед не тронут");
        assert!(!history.can_undo() && history.can_redo());

        let label = history.redo(&mut scene, now).expect("повтор");
        assert_eq!(label, "позиция");
        assert_eq!(scene.objects[0].position.x, 5.0, "«после» вернулось");
        assert!(history.can_undo() && !history.can_redo());
    }

    #[test]
    fn add_and_remove_round_trip() {
        let mut scene = scene_with_objects(2);
        let mut history = History::default();
        let now = Instant::now();

        // Размещение: объект в конце, откат убирает, повтор ставит обратно.
        let placed = SceneObject::new(Vec3::new(9.0, 0.0, 0.0), Vec3::ONE, Vec4::ONE);
        scene.add(placed.clone());
        history.push(EditCommand::add(2, &placed), now);
        assert_eq!(scene.len(), 3);

        history.undo(&mut scene, now).expect("откат размещения");
        assert_eq!(scene.len(), 2);
        history.redo(&mut scene, now).expect("повтор размещения");
        assert_eq!(scene.len(), 3);
        assert_eq!(scene.objects[2].position.x, 9.0, "объект вернулся на место");

        // Удаление середины: откат вставляет обратно на прежний индекс.
        let removed = scene.objects.remove(1);
        history.push(EditCommand::remove(1, &removed), now);
        assert_eq!(scene.len(), 2);
        assert_eq!(scene.objects[1].position.x, 9.0, "после удаления сдвиг");

        history.undo(&mut scene, now).expect("откат удаления");
        assert_eq!(scene.len(), 3);
        assert_eq!(scene.objects[1].position.x, 1.0, "вернулся на индекс 1");
        history.redo(&mut scene, now).expect("повтор удаления");
        assert_eq!(scene.len(), 2);
        assert_eq!(scene.objects[1].position.x, 9.0);
    }

    #[test]
    fn consecutive_changes_coalesce_into_one_command() {
        let mut scene = scene_with_objects(1);
        let mut history = History::default();
        let start = Instant::now();

        // Серия кадровых правок одного поля — как при перетаскивании DragValue.
        let mut object = scene.objects[0].clone();
        for (step, x) in [1.0_f32, 2.0, 3.0].iter().enumerate() {
            let before = object.clone();
            object.position.x = *x;
            scene.objects[0] = object.clone();
            let now = start + Duration::from_millis(step as u64 * 100);
            history.push(EditCommand::change("позиция", 0, &before, &object), now);
        }

        assert_eq!(history.depth(), 1, "серия правок склеена");
        history.undo(&mut scene, start).expect("откат");
        assert_eq!(scene.objects[0].position.x, 0.0, "вернулись к исходному");
        history.redo(&mut scene, start).expect("повтор");
        assert_eq!(scene.objects[0].position.x, 3.0, "повторилась вся серия");
    }

    #[test]
    fn changes_do_not_merge_after_window_or_between_fields() {
        let scene = scene_with_objects(1);
        let mut history = History::default();
        let start = Instant::now();
        let before = scene.objects[0].clone();
        let after = moved(&before, 1.0);

        history.push(EditCommand::change("позиция", 0, &before, &after), start);
        // Та же правка, но спустя окно склейки — отдельная команда.
        history.push(
            EditCommand::change("позиция", 0, &after, &moved(&after, 2.0)),
            start + COALESCE_WINDOW + Duration::from_millis(1),
        );
        assert_eq!(history.depth(), 2, "пауза разрывает серию");

        // Другое поле рядом по времени — тоже отдельная команда.
        let before = moved(&after, 2.0);
        let mut scaled = before.clone();
        scaled.scale = Vec3::splat(2.0);
        history.push(EditCommand::change("масштаб", 0, &before, &scaled), start);
        assert_eq!(history.depth(), 3, "разные поля не склеиваются");
    }

    #[test]
    fn push_clears_redo_and_limit_drops_oldest() {
        let mut scene = scene_with_objects(1);
        let mut history = History::new(2);
        let now = Instant::now();

        let first = scene.objects[0].clone();
        let second = moved(&first, 1.0);
        history.push(EditCommand::change("позиция", 0, &first, &second), now);
        scene.objects[0] = second.clone();
        history.undo(&mut scene, now).expect("откат");
        assert!(history.can_redo());
        // Новая правка после отката стирает redo.
        history.push(
            EditCommand::change("масштаб", 0, &first, &second),
            now + Duration::from_secs(1),
        );
        assert!(!history.can_redo(), "redo сброшен новой правкой");

        // Предел глубины: старая команда вытесняется.
        history.clear();
        for step in 0..3 {
            history.push(
                EditCommand::change("позиция", 0, &first, &second),
                now + Duration::from_secs(10 + step),
            );
        }
        assert_eq!(history.depth(), 2, "глубина ограничена");
    }

    #[test]
    fn undo_survives_object_removed_outside_history() {
        let mut scene = scene_with_objects(1);
        let mut history = History::default();
        let now = Instant::now();
        let before = scene.objects[0].clone();
        let after = moved(&before, 1.0);
        history.push(EditCommand::change("позиция", 0, &before, &after), now);

        // Объект исчез (например, сцена перезагружена) — откат не паникует.
        scene.objects.clear();
        assert!(history.undo(&mut scene, now).is_some());
        assert!(scene.objects.is_empty());
    }

    #[test]
    fn object_edit_label_names_first_changed_field() {
        let base = SceneObject::new(Vec3::ZERO, Vec3::ONE, Vec4::ONE);
        let mut position = base.clone();
        position.position.x = 1.0;
        assert_eq!(object_edit_label(&base, &position), "позиция");

        let mut texture = base.clone();
        texture.texture = Some("builtin://concrete".into());
        assert_eq!(object_edit_label(&base, &texture), "текстура");

        let mut rough = base.clone();
        rough.roughness = Some(0.5);
        assert_eq!(object_edit_label(&base, &rough), "шероховатость");

        let mut uv = base.clone();
        uv.world_uv = true;
        assert_eq!(object_edit_label(&base, &uv), "текстура (UV)");

        assert_eq!(object_edit_label(&base, &base), "объект", "без правок");
    }

    fn foggy(environment: &SceneEnvironment) -> SceneEnvironment {
        let mut after = environment.clone();
        after.fog_density = 0.05;
        after.fog_color = Vec3::new(0.5, 0.5, 0.5);
        after
    }

    #[test]
    fn environment_undo_and_redo_round_trip() {
        let mut scene = Scene::new("тест");
        let before = scene.environment.clone();
        let after = foggy(&before);
        scene.environment = after.clone();

        let mut history = History::default();
        let now = Instant::now();
        history.push(EditCommand::environment("туман", &before, &after), now);

        assert_eq!(history.undo_label(), Some("туман"));
        let label = history.undo(&mut scene, now).expect("откат");
        assert_eq!(label, "туман");
        assert_eq!(scene.environment, before, "«до» восстановлено");
        assert_eq!(scene.environment.fog_density, 0.0);

        let label = history.redo(&mut scene, now).expect("повтор");
        assert_eq!(label, "туман");
        assert_eq!(scene.environment, after, "«после» вернулось");
    }

    #[test]
    fn environment_changes_coalesce_like_drags() {
        let mut scene = Scene::new("тест");
        let mut history = History::default();
        let start = Instant::now();

        // Серия кадровых правок одного поля — как при перетаскивании DragValue.
        let original = scene.environment.clone();
        let mut environment = original.clone();
        for (step, density) in [0.01_f32, 0.02, 0.03].iter().enumerate() {
            let before = environment.clone();
            environment.fog_density = *density;
            scene.environment = environment.clone();
            let now = start + Duration::from_millis(step as u64 * 100);
            history.push(
                EditCommand::environment("туман (плотность)", &before, &environment),
                now,
            );
        }

        assert_eq!(history.depth(), 1, "серия правок склеена");
        history.undo(&mut scene, start).expect("откат");
        assert_eq!(scene.environment, original, "вернулись к исходному");
        history.redo(&mut scene, start).expect("повтор");
        assert_eq!(scene.environment.fog_density, 0.03, "повторилась серия");
    }

    #[test]
    fn different_environment_fields_do_not_merge() {
        let scene = Scene::new("тест");
        let mut history = History::default();
        let now = Instant::now();
        let base = scene.environment.clone();

        let mut fog = base.clone();
        fog.fog_density = 0.02;
        history.push(
            EditCommand::environment("туман (плотность)", &base, &fog),
            now,
        );

        let mut sun = fog.clone();
        sun.sun_intensity = 2.0;
        history.push(
            EditCommand::environment("солнце (яркость)", &fog, &sun),
            now,
        );

        assert_eq!(history.depth(), 2, "разные поля — разные команды");
    }

    #[test]
    fn environment_edit_label_names_field() {
        let base = SceneEnvironment::default();
        let mut sun = base.clone();
        sun.sun_color = Vec3::new(1.0, 0.5, 0.2);
        assert_eq!(environment_edit_label(&base, &sun), "солнце (цвет)");

        let mut fog = base.clone();
        fog.fog_density = 0.01;
        assert_eq!(environment_edit_label(&base, &fog), "туман (плотность)");

        let mut sky = base.clone();
        sky.sky_top = Vec3::new(0.2, 0.3, 0.6);
        assert_eq!(environment_edit_label(&base, &sky), "небо (зенит)");

        assert_eq!(
            environment_edit_label(&base, &base),
            "атмосфера",
            "без правок"
        );
    }

    #[test]
    fn same_field_changes_of_different_objects_do_not_merge() {
        // Панель свойств правит выделенный объект; при переключении выделения
        // правки того же поля у РАЗНЫХ объектов не должны склеиваться.
        let mut scene = scene_with_objects(2);
        let mut history = History::default();
        let now = Instant::now();

        let first_before = scene.objects[0].clone();
        let first_after = moved(&first_before, 1.0);
        scene.objects[0] = first_after.clone();
        history.push(
            EditCommand::change("позиция", 0, &first_before, &first_after),
            now,
        );

        let second_before = scene.objects[1].clone();
        let second_after = moved(&second_before, 8.0);
        scene.objects[1] = second_after.clone();
        history.push(
            EditCommand::change("позиция", 1, &second_before, &second_after),
            now,
        );

        assert_eq!(history.depth(), 2, "разные объекты — разные команды");
        history.undo(&mut scene, now).expect("откат второй");
        assert_eq!(scene.objects[1].position.x, 1.0, "второй объект вернулся");
        assert_eq!(scene.objects[0].position.x, 1.0, "первый не тронут");
    }

    #[test]
    fn undo_chain_restores_scene_to_original() {
        // Сессия правок как в редакторе: свойство → размещение → удаление →
        // ещё свойство. undo до конца возвращает исходную сцену, redo — финальную.
        let original = scene_with_objects(2);
        let mut scene = original.clone();
        let mut history = History::default();
        let mut now = Instant::now();

        // 1. Правка свойства объекта 0.
        let before = scene.objects[0].clone();
        scene.objects[0] = moved(&before, 3.0);
        let after_move = scene.objects[0].clone();
        history.push(EditCommand::change("позиция", 0, &before, &after_move), now);
        now += Duration::from_secs(1);

        // 2. Размещение нового объекта.
        let placed = SceneObject::new(Vec3::new(9.0, 0.0, 0.0), Vec3::ONE, Vec4::ONE);
        scene.add(placed.clone());
        history.push(EditCommand::add(2, &placed), now);
        now += Duration::from_secs(1);

        // 3. Удаление объекта 1.
        let removed = scene.objects.remove(1);
        history.push(EditCommand::remove(1, &removed), now);
        now += Duration::from_secs(1);

        // 4. Правка свойства у оставшегося объекта 0.
        let before = scene.objects[0].clone();
        let mut scaled = before.clone();
        scaled.scale = Vec3::splat(2.0);
        scene.objects[0] = scaled.clone();
        history.push(EditCommand::change("масштаб", 0, &before, &scaled), now);

        let final_objects = scene.objects.clone();
        assert_eq!(history.depth(), 4);

        // undo всей цепочки — сцена как исходная.
        for _ in 0..4 {
            assert!(history.undo(&mut scene, now).is_some());
        }
        assert_eq!(
            scene.objects, original.objects,
            "цепочка откатов вернула исходник"
        );
        assert!(!history.can_undo() && history.can_redo());

        // redo всей цепочки — сцена как финальная.
        for _ in 0..4 {
            assert!(history.redo(&mut scene, now).is_some());
        }
        assert_eq!(
            scene.objects, final_objects,
            "цепочка повторов вернула финал"
        );
        assert!(history.can_undo() && !history.can_redo());
    }
}
