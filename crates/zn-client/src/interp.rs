//! Интерполяция удалённых игроков: буфер снапшотов на игрока и сэмпл на
//! момент рендера (новейший снапшот минус `INTERP_DELAY_TICKS`).

use std::collections::VecDeque;

use glam::{Vec3, Vec4};
use zn_net::{PlayerNetState, SIMULATION_HZ};

/// Вместимость буфера на игрока: 64 снапшота ≈ 2 с при 30 Гц.
pub const INTERP_BUFFER_CAP: usize = 64;

/// Задержка рендера удалённых игроков — 100 мс, выраженная в тиках
/// симуляции (при 60 Гц это 6 тиков).
pub const INTERP_DELAY_TICKS: u64 = SIMULATION_HZ as u64 / 10;

/// Позиция удалённого игрока для кадра (мировые координаты, ступни).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerSample {
    pub position: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    /// Здоровье; 0 — мёртв и не рисуется.
    pub health: u16,
}

/// Буфер интерполяции одного удалённого игрока.
#[derive(Debug)]
pub struct InterpolatedPlayer {
    player_id: u64,
    /// Снапшоты по возрастанию `server_tick`.
    buffer: VecDeque<(u64, PlayerNetState)>,
    starvation_warned: bool,
}

impl InterpolatedPlayer {
    pub fn new(player_id: u64) -> Self {
        Self {
            player_id,
            buffer: VecDeque::new(),
            starvation_warned: false,
        }
    }

    /// Кладёт снапшот; устаревшие/дублирующие ticks отбрасываются.
    pub fn push(&mut self, server_tick: u64, state: PlayerNetState) {
        if let Some((newest, _)) = self.buffer.back() {
            if server_tick <= *newest {
                return;
            }
            // Свежие данные пришли — голодание кончилось.
            self.starvation_warned = false;
        }
        self.buffer.push_back((server_tick, state));
        while self.buffer.len() > INTERP_BUFFER_CAP {
            self.buffer.pop_front();
        }
    }

    /// Сэмпл на момент `render_tick`:
    /// - пара вокруг → линейная интерполяция (yaw — по кратчайшей дуге);
    /// - новее самого свежего снапшота → голодание: держим свежайший сэмпл
    ///   (один warn на игрока);
    /// - старее самого старого → держим старейший.
    pub fn sample(&mut self, render_tick: u64) -> Option<PlayerSample> {
        let newest = self.buffer.back()?.0;
        if render_tick >= newest {
            if render_tick > newest && !self.starvation_warned {
                self.starvation_warned = true;
                log::warn!(
                    "игрок {}: нет пары снапшотов вокруг рендер-тика {render_tick} \
                     (свежайший {newest}) — держим последнюю позицию",
                    self.player_id
                );
            }
            let state = &self.buffer.back()?.1;
            return Some(sample_of(state));
        }

        let mut before: Option<&(u64, PlayerNetState)> = None;
        let mut after: Option<&(u64, PlayerNetState)> = None;
        for entry in &self.buffer {
            if entry.0 <= render_tick {
                before = Some(entry);
            } else {
                after = Some(entry);
                break;
            }
        }

        let Some((t0, s0)) = before else {
            // Рендер-тик старее всего буфера — держим старейший снапшот.
            let state = &self.buffer.front()?.1;
            return Some(sample_of(state));
        };
        let Some((t1, s1)) = after else {
            // Недостижимо (render_tick < newest), но защищаемся.
            return Some(sample_of(s0));
        };

        let alpha = (render_tick - t0) as f32 / (t1 - t0) as f32;
        let p0 = Vec3::from_array(s0.position);
        let p1 = Vec3::from_array(s1.position);
        Some(PlayerSample {
            position: p0.lerp(p1, alpha),
            yaw: lerp_angle(s0.yaw, s1.yaw, alpha),
            pitch: s0.pitch + (s1.pitch - s0.pitch) * alpha,
            health: s0.health,
        })
    }
}

fn sample_of(state: &PlayerNetState) -> PlayerSample {
    PlayerSample {
        position: Vec3::from_array(state.position),
        yaw: state.yaw,
        pitch: state.pitch,
        health: state.health,
    }
}

/// Интерполяция угла по кратчайшей дуге (переход через ±π не «прокручивает»).
fn lerp_angle(from: f32, to: f32, alpha: f32) -> f32 {
    let mut delta = (to - from) % std::f32::consts::TAU;
    if delta > std::f32::consts::PI {
        delta -= std::f32::consts::TAU;
    }
    if delta < -std::f32::consts::PI {
        delta += std::f32::consts::TAU;
    }
    (from + delta * alpha).rem_euclid(std::f32::consts::TAU)
}

/// Детерминированный цвет капсулы игрока: оттенок по золотому сечению
/// (равномерный разнос), одинаков у всех клиентов.
pub fn player_color(player_id: u64) -> Vec4 {
    let hue = (player_id as f32 * 0.618_034).fract();
    let (r, g, b) = hsv_to_rgb(hue, 0.65, 0.9);
    Vec4::new(r, g, b, 1.0)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let h = (h.rem_euclid(1.0)) * 6.0;
    let sector = h.floor();
    let f = h - sector;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match sector as i32 % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(x: f32, yaw: f32) -> PlayerNetState {
        PlayerNetState {
            player_id: 7,
            position: [x, 0.5, 0.0],
            velocity: [0.0; 3],
            yaw,
            pitch: 0.0,
            health: zn_sim::MAX_HEALTH,
            flags: 0,
        }
    }

    fn player() -> InterpolatedPlayer {
        let mut player = InterpolatedPlayer::new(7);
        player.push(10, state(0.0, 0.0));
        player.push(20, state(10.0, 0.0));
        player
    }

    #[test]
    fn samples_midpoint_between_snapshots() {
        let mut player = player();
        let sample = player.sample(15).unwrap();
        assert!((sample.position.x - 5.0).abs() < 1e-5, "{sample:?}");

        let at_start = player.sample(10).unwrap();
        assert!((at_start.position.x - 0.0).abs() < 1e-5);
        let at_end = player.sample(20).unwrap();
        assert!((at_end.position.x - 10.0).abs() < 1e-5);
    }

    #[test]
    fn holds_newest_and_warns_on_starvation() {
        let mut player = player();
        let sample = player.sample(25).unwrap();
        assert!((sample.position.x - 10.0).abs() < 1e-5, "держим свежайший");
        // Второй вызов не должен ломаться (warn уже был).
        assert!(player.sample(30).is_some());
    }

    #[test]
    fn holds_oldest_when_render_tick_is_behind() {
        let mut player = player();
        let sample = player.sample(5).unwrap();
        assert!((sample.position.x - 0.0).abs() < 1e-5, "держим старейший");
    }

    #[test]
    fn yaw_interpolates_over_shortest_arc() {
        let mut player = InterpolatedPlayer::new(1);
        player.push(0, state(0.0, 3.0));
        player.push(10, state(0.0, -3.0));

        // Кратчайшая дуга 3.0 → -3.0 идёт через π, а не через 0.
        let mid = player.sample(5).unwrap();
        let delta_from_pi = (mid.yaw - std::f32::consts::PI).abs();
        assert!(delta_from_pi < 0.2, "yaw = {}", mid.yaw);
    }

    #[test]
    fn buffer_caps_and_drops_stale_ticks() {
        let mut player = InterpolatedPlayer::new(1);
        for tick in 0..(INTERP_BUFFER_CAP as u64 + 10) {
            player.push(tick, state(tick as f32, 0.0));
        }

        // Вытеснены старейшие: на рендер-тик 0 буфер отдаёт tick 10.
        let oldest = player.sample(0).unwrap();
        assert!((oldest.position.x - 10.0).abs() < 1e-5, "{oldest:?}");
        // Свежайший снапшот на месте.
        let newest_tick = INTERP_BUFFER_CAP as u64 + 9;
        let newest = player.sample(newest_tick).unwrap();
        assert!((newest.position.x - newest_tick as f32).abs() < 1e-5);

        // Устаревший tick игнорируется (иначе новейшим стал бы x = 999).
        player.push(1, state(999.0, 0.0));
        let sample = player.sample(10).unwrap();
        assert!((sample.position.x - 10.0).abs() < 1e-5, "{sample:?}");
    }

    #[test]
    fn player_colors_are_deterministic_and_distinct() {
        let color = player_color(1);
        assert_eq!(color, player_color(1));
        assert_ne!(player_color(1), player_color(2));
        for id in 0..16 {
            let c = player_color(id);
            assert!((0.0..=1.0).contains(&c.x));
            assert!((0.0..=1.0).contains(&c.y));
            assert!((0.0..=1.0).contains(&c.z));
            assert_eq!(c.w, 1.0);
        }
    }
}
