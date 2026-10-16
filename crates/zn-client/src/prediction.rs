//! Prediction/reconciliation локального игрока: история инпутов, реплей после
//! снапшота сервера и визуальное сглаживание поправки.

use std::collections::VecDeque;

use glam::Vec3;
use zn_net::{ClientInput, PlayerNetState};
use zn_sim::Simulation;

/// История инпутов локального игрока: 256 шагов ≈ 4.3 с при 60 Гц — перекрывает
/// любой разумный лаг снапшота.
pub const INPUT_HISTORY_CAP: usize = 256;

/// Время экспоненциального гашения визуальной поправки, с.
pub const SMOOTH_DECAY_SECONDS: f32 = 0.15;
/// Поправка больше этого — мгновенный снап без сглаживания, м.
pub const SNAP_THRESHOLD_M: f32 = 2.0;
/// Поправка меньше этого — игнорируется (шум плавающей точки), м.
pub const IGNORE_THRESHOLD_M: f32 = 0.01;

/// Кольцевая история инпутов, отправленных серверу (для реплея).
#[derive(Clone, Debug, Default)]
pub struct InputHistory {
    entries: VecDeque<ClientInput>,
}

impl InputHistory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, input: ClientInput) {
        self.entries.push_back(input);
        while self.entries.len() > INPUT_HISTORY_CAP {
            self.entries.pop_front();
        }
    }

    /// Последние `n` инпутов в порядке отправки (для input-пакета).
    pub fn last_n(&self, n: usize) -> Vec<ClientInput> {
        let skip = self.entries.len().saturating_sub(n);
        self.entries.iter().skip(skip).copied().collect()
    }

    /// Инпуты новее `tick` (то, что сервер ещё не учёл — реплей).
    pub fn since(&self, tick: u64) -> Vec<ClientInput> {
        self.entries
            .iter()
            .filter(|input| input.tick > tick)
            .copied()
            .collect()
    }
}

/// Результат reconciliation: до и после коррекции (позиции ступней).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReconcileOutcome {
    pub predicted: Vec3,
    pub corrected: Vec3,
    pub replayed: usize,
}

impl ReconcileOutcome {
    /// Визуальная поправка: сколько нужно довести рендер (predicted − corrected).
    pub fn error(&self) -> Vec3 {
        self.predicted - self.corrected
    }
}

/// Восстанавливает состояние игрока из снапшота сервера и переигрывает
/// инпуты новее `acked_input_tick` из истории.
/// `None`, если игрока нет в локальной симуляции.
pub fn reconcile(
    simulation: &mut Simulation,
    player_id: u64,
    server_state: &PlayerNetState,
    acked_input_tick: u64,
    history: &InputHistory,
) -> Option<ReconcileOutcome> {
    let predicted = simulation.player_state(player_id)?.position;
    if !simulation.restore_player(player_id, server_state) {
        return None;
    }
    simulation.clear_inputs(player_id);

    let pending = history.since(acked_input_tick);
    for input in &pending {
        simulation.queue_input(player_id, *input);
    }
    for _ in 0..pending.len() {
        simulation.step_once();
    }

    let corrected = simulation.player_state(player_id)?.position;
    Some(ReconcileOutcome {
        predicted,
        corrected,
        replayed: pending.len(),
    })
}

/// Экспоненциально гаснущее смещение рендера после поправки сервера.
/// Большая поправка (> `SNAP_THRESHOLD_M`) не сглаживается — снап сразу.
#[derive(Clone, Copy, Debug, Default)]
pub struct ErrorSmoother {
    offset: Vec3,
}

impl ErrorSmoother {
    pub fn offset(&self) -> Vec3 {
        self.offset
    }

    /// Учитывает новую поправку `predicted − corrected`.
    pub fn on_correction(&mut self, error: Vec3) {
        let length = error.length();
        if length < IGNORE_THRESHOLD_M {
            return;
        }
        if length > SNAP_THRESHOLD_M {
            self.offset = Vec3::ZERO;
            return;
        }
        self.offset += error;
    }

    /// Гасит накопленное смещение за `dt` секунд.
    pub fn decay(&mut self, dt: f32) {
        if dt <= 0.0 {
            return;
        }
        self.offset *= (-dt / SMOOTH_DECAY_SECONDS).exp();
        if self.offset.length_squared() < 1e-8 {
            self.offset = Vec3::ZERO;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zn_scene::{Scene, SceneObject};

    fn input(tick: u64, forward: f32) -> ClientInput {
        ClientInput {
            tick,
            move_forward: forward,
            ..ClientInput::new(tick)
        }
    }

    #[test]
    fn history_keeps_last_inputs_in_order() {
        let mut history = InputHistory::new();
        for tick in 0..10 {
            history.push(input(tick, 1.0));
        }
        let last = history.last_n(4);
        let ticks: Vec<u64> = last.iter().map(|input| input.tick).collect();
        assert_eq!(ticks, vec![6, 7, 8, 9]);

        let since = history.since(7);
        let ticks: Vec<u64> = since.iter().map(|input| input.tick).collect();
        assert_eq!(ticks, vec![8, 9]);

        assert_eq!(history.last_n(100).len(), 10);
    }

    #[test]
    fn history_caps_at_limit() {
        let mut history = InputHistory::new();
        for tick in 0..(INPUT_HISTORY_CAP as u64 + 50) {
            history.push(input(tick, 1.0));
        }
        let all = history.last_n(usize::MAX);
        assert_eq!(all.len(), INPUT_HISTORY_CAP);
        assert_eq!(all.last().unwrap().tick, INPUT_HISTORY_CAP as u64 + 49);
    }

    /// Сцена-стенка: пол и стена перед спавном (yaw = 0 смотрит в +X).
    fn wall_scene() -> Scene {
        let mut scene = Scene::new("wall");
        scene.spawn = Vec3::new(0.0, 0.0, 0.0);
        scene.spawn_yaw = 0.0;
        scene.add(SceneObject::new(
            Vec3::new(0.0, -0.5, 0.0),
            Vec3::new(20.0, 0.5, 20.0),
            glam::Vec4::ONE,
        ));
        scene.add(SceneObject::new(
            Vec3::new(3.0, 1.0, 0.0),
            Vec3::new(0.5, 2.0, 4.0),
            glam::Vec4::ONE,
        ));
        scene
    }

    #[test]
    fn replay_after_restore_matches_prediction_exactly() {
        let scene = wall_scene();
        // «Сервер»: 40 шагов вперёд к стене.
        let mut server = Simulation::with_scene(&scene);
        let server_id = server.spawn_player(scene.spawn);
        let mut history = InputHistory::new();
        for tick in 0..40u64 {
            let command = input(tick, 1.0);
            history.push(command);
            server.queue_input(server_id, command);
            server.step_once();
        }
        let predicted = server.player_state(server_id).unwrap().position;

        // Снапшот сервера после 20-го шага (ack = tick 19).
        let mut mid_server = Simulation::with_scene(&scene);
        let mid_id = mid_server.spawn_player(scene.spawn);
        for tick in 0..20u64 {
            let command = input(tick, 1.0);
            mid_server.queue_input(mid_id, command);
            mid_server.step_once();
        }
        let snapshot = mid_server.player_state(mid_id).unwrap().net_state();

        // «Клиент»: восстановление из снапшота + реплей истории (20..39).
        let mut client = Simulation::with_scene(&scene);
        let client_id = client.spawn_player(scene.spawn);
        let outcome = reconcile(&mut client, client_id, &snapshot, 19, &history).unwrap();
        assert_eq!(outcome.replayed, 20);
        assert!(
            (outcome.corrected - predicted).length() < 1e-4,
            "реплей разошёлся: {:?} vs {predicted:?}",
            outcome.corrected
        );
    }

    #[test]
    fn reconcile_ignores_inputs_already_acked() {
        let scene = wall_scene();
        let mut simulation = Simulation::with_scene(&scene);
        let id = simulation.spawn_player(scene.spawn);
        let mut history = InputHistory::new();
        for tick in 0..5u64 {
            let command = input(tick, 1.0);
            history.push(command);
            simulation.queue_input(id, command);
            simulation.step_once();
        }
        let snapshot = simulation.player_state(id).unwrap().net_state();

        // ack = 4 — реплеить нечего.
        let outcome = reconcile(&mut simulation, id, &snapshot, 4, &history).unwrap();
        assert_eq!(outcome.replayed, 0);
        assert!(outcome.error().length() < 1e-5);
    }

    #[test]
    fn smoother_snaps_on_big_error_and_ignores_noise() {
        let mut smoother = ErrorSmoother::default();
        smoother.on_correction(Vec3::new(0.005, 0.0, 0.0));
        assert_eq!(smoother.offset(), Vec3::ZERO, "шум игнорируется");

        smoother.on_correction(Vec3::new(3.0, 0.0, 0.0));
        assert_eq!(
            smoother.offset(),
            Vec3::ZERO,
            "большая поправка — мгновенный снап"
        );

        smoother.on_correction(Vec3::new(0.5, 0.0, 0.0));
        assert!((smoother.offset().length() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn smoother_decays_exponentially() {
        let mut smoother = ErrorSmoother::default();
        smoother.on_correction(Vec3::new(0.5, 0.0, 0.0));
        smoother.decay(SMOOTH_DECAY_SECONDS);
        let expected = 0.5 * (-1.0f32).exp();
        assert!((smoother.offset().length() - expected).abs() < 1e-4);

        smoother.decay(2.0);
        assert!(
            smoother.offset().length() < 0.01,
            "остаток {:?}",
            smoother.offset()
        );
    }
}
