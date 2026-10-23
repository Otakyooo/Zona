//! Аудио-слой (A1): менеджер kira + процедурные звуки из [`procedural`].
//!
//! Движок безопасно деградирует без устройства звука (CI, headless):
//! `play` становится no-op, игра продолжается без звука.

mod procedural;

use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use kira::{
    sound::static_sound::{StaticSoundData, StaticSoundSettings},
    AudioManager, AudioManagerSettings, Decibels, DefaultBackend, Value,
};

/// Сериализует создание `AudioManager`: kira/cpal-WASAPI на машинах без
/// аудио-устройства при повторных/параллельных init-ах валит процесс
/// (STATUS_ACCESS_VIOLATION на CI-раннерах). Один init на процесс —
/// детерминированно и в headless.
static AUDIO_INIT: Mutex<()> = Mutex::new(());

/// init уже пробовали и устройство не поднялось — больше не пробуем:
/// на headless каждый повторный init роняет процесс целиком
/// (STATUS_ACCESS_VIOLATION). Флаг читается/пишется только под
/// [`AUDIO_INIT`], поэтому обычный `Ordering` достаточен.
static AUDIO_INIT_FAILED: AtomicBool = AtomicBool::new(false);

/// Звуки игры. `Extract` заведён под круги экстракции (M4d-2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SoundId {
    /// Выстрел (свой или чужой).
    Shot,
    /// Попадание по игроку.
    Hit,
    /// Подтверждение попадания (звонкий щелчок хитмаркера).
    HitConfirm,
    /// Подбор/выброс предмета.
    Pickup,
    /// Бросок болта.
    Throw,
    /// Болт сработал в аномалии.
    BoltAnomaly,
    /// Лечение (бинт/аптечка).
    Heal,
    /// Смерть игрока.
    Death,
    /// Экстракция (чойс).
    Extract,
    /// Интерфейс: панель, инвентарь, склад.
    Panel,
    /// Переход между локациями.
    Transition,
    /// Шаг по бетону.
    StepConcrete,
    /// Шаг по земле/траве.
    StepDirt,
    /// Шаг по металлу.
    StepMetal,
    /// Шаг по асфальту.
    StepAsphalt,
    /// Рычание мутанта (агро).
    MutantGrowl,
    /// Укус мутанта (атака).
    MutantBite,
    /// Визг / получение урона мутантом.
    MutantPain,
}

impl SoundId {
    /// Все звуки — для тестов и отладки.
    pub const ALL: [SoundId; 18] = [
        SoundId::Shot,
        SoundId::Hit,
        SoundId::HitConfirm,
        SoundId::Pickup,
        SoundId::Throw,
        SoundId::BoltAnomaly,
        SoundId::Heal,
        SoundId::Death,
        SoundId::Extract,
        SoundId::Panel,
        SoundId::Transition,
        SoundId::StepConcrete,
        SoundId::StepDirt,
        SoundId::StepMetal,
        SoundId::StepAsphalt,
        SoundId::MutantGrowl,
        SoundId::MutantBite,
        SoundId::MutantPain,
    ];

    /// Ключ для логов и настроек.
    pub fn key(self) -> &'static str {
        match self {
            SoundId::Shot => "shot",
            SoundId::Hit => "hit",
            SoundId::HitConfirm => "hit_confirm",
            SoundId::Pickup => "pickup",
            SoundId::Throw => "throw",
            SoundId::BoltAnomaly => "bolt_anomaly",
            SoundId::Heal => "heal",
            SoundId::Death => "death",
            SoundId::Extract => "extract",
            SoundId::Panel => "panel",
            SoundId::Transition => "transition",
            SoundId::StepConcrete => "step_concrete",
            SoundId::StepDirt => "step_dirt",
            SoundId::StepMetal => "step_metal",
            SoundId::StepAsphalt => "step_asphalt",
            SoundId::MutantGrowl => "mutant_growl",
            SoundId::MutantBite => "mutant_bite",
            SoundId::MutantPain => "mutant_pain",
        }
    }
}

/// Шаги громкости (F9): линейная доля, 0 — тишина.
pub const VOLUME_STEPS: [f32; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];

/// Громкость по умолчанию.
pub const DEFAULT_VOLUME: f32 = 0.7;

/// Линейная доля → децибелы kira (минус бесконечность не нужен: тишина отсекается).
fn amplitude_to_db(volume: f32) -> f32 {
    20.0 * volume.max(1e-3).log10()
}

/// Аудио-движок: воспроизведение процедурных звуков, громкость и мьют.
/// Без устройства звука работает «молча» — вызовы безопасны.
pub struct AudioEngine {
    manager: Option<AudioManager<DefaultBackend>>,
    muted: bool,
    volume: f32,
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self {
            manager: None,
            muted: false,
            volume: DEFAULT_VOLUME,
        }
    }
}

impl AudioEngine {
    pub fn new() -> Self {
        Self::default().initialized()
    }

    /// Пытается поднять устройство звука; при неудаче — молчаливый режим.
    /// Создание сериализовано (см. [`AUDIO_INIT`]): на headless-машинах
    /// повторный init kira роняет процесс целиком. После первой неудачной
    /// попытки (см. [`AUDIO_INIT_FAILED`]) init не повторяется вовсе —
    /// поэтому zn-client-тесты (десятки `AudioEngine::new()` в параллели)
    /// на CI не валят процесс.
    pub fn initialized(mut self) -> Self {
        let guard = AUDIO_INIT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if AUDIO_INIT_FAILED.load(Ordering::Relaxed) {
            drop(guard);
            return self;
        }
        match AudioManager::<DefaultBackend>::new(AudioManagerSettings::default()) {
            Ok(manager) => {
                log::info!("audio engine initialized");
                self.manager = Some(manager);
            }
            Err(error) => {
                // Больше не пробуем: на машинах без устройства каждый
                // следующий init — STATUS_ACCESS_VIOLATION процесса.
                AUDIO_INIT_FAILED.store(true, Ordering::Relaxed);
                log::warn!("audio device unavailable ({error}); running muted");
            }
        }
        drop(guard);
        self
    }

    /// Устройство звука поднято (иначе воспроизведение — no-op).
    pub fn enabled(&self) -> bool {
        self.manager.is_some()
    }

    pub fn muted(&self) -> bool {
        self.muted
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }

    /// Текущая громкость (0..=1).
    pub fn volume(&self) -> f32 {
        self.volume
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
    }

    /// Следующий шаг громкости по циклу VOLUME_STEPS; возвращает новое значение.
    pub fn cycle_volume(&mut self) -> f32 {
        let next = VOLUME_STEPS
            .iter()
            .copied()
            .find(|step| *step > self.volume + f32::EPSILON)
            .unwrap_or(VOLUME_STEPS[0]);
        self.volume = next;
        next
    }

    /// Играет звук: учитываются мьют и громкость, отсутствие устройства.
    pub fn play(&mut self, sound: SoundId) {
        if self.muted || self.volume <= 0.0 {
            return;
        }
        let Some(manager) = self.manager.as_mut() else {
            return;
        };
        let settings =
            StaticSoundSettings::new().volume(Value::Fixed(Decibels(amplitude_to_db(self.volume))));
        let Ok(data) = StaticSoundData::from_cursor(Cursor::new(procedural::wav_bytes(sound)))
        else {
            log::warn!("audio: не удалось декодировать {}", sound.key());
            return;
        };
        let data = data.with_settings(settings);
        if let Err(error) = manager.play(data) {
            log::warn!("audio: play {} — {error}", sound.key());
        }
    }
}

impl std::fmt::Debug for AudioEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioEngine")
            .field("enabled", &self.enabled())
            .field("muted", &self.muted)
            .field("volume", &self.volume)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ВАЖНО: реальный init (AudioEngine::new) делает ТОЛЬКО тест ниже — но
    // теперь это безопасно и для остальных: на машинах без аудио-устройства
    // (CI-раннеры) первая неудачная попытка выставляет AUDIO_INIT_FAILED, и
    // все повторные init сразу уходят в молчаливый режим. Раньше kira на
    // headless при повторной инициализации валил весь процесс
    // (STATUS_ACCESS_VIOLATION) — именно на этом падал CI в тестах
    // zn-client (67 тестов, каждый со своим AudioEngine::new()).

    #[test]
    fn engine_initializes_or_fails_softly() {
        let engine = AudioEngine::new();
        println!(
            "audio enabled: {} volume: {} muted: {}",
            engine.enabled(),
            engine.volume(),
            engine.muted()
        );
        assert_eq!(engine.volume(), DEFAULT_VOLUME);
    }

    #[test]
    fn repeated_init_does_not_crash_headless() {
        // Регрессия на CI: повторные init на машине без устройства валили
        // процесс (STATUS_ACCESS_VIOLATION). Подряд два движка — на CI
        // второй уходит в mute по флагу, локально оба поднимают устройство.
        let first = AudioEngine::new();
        let second = AudioEngine::new();
        assert_eq!(first.volume(), second.volume());
        // Флаг недоступности общий на процесс: состояния совпадают.
        assert_eq!(first.enabled(), second.enabled());
    }

    #[test]
    fn volume_cycles_and_clamps() {
        let mut engine = AudioEngine::default();
        engine.set_volume(0.7);
        assert_eq!(engine.cycle_volume(), 0.75);
        assert_eq!(engine.cycle_volume(), 1.0);
        assert_eq!(engine.cycle_volume(), 0.0, "цикл заворачивается");
        engine.set_volume(2.5);
        assert_eq!(engine.volume(), 1.0);
        engine.set_volume(-1.0);
        assert_eq!(engine.volume(), 0.0);
    }

    #[test]
    fn muted_or_silent_engine_is_noop() {
        let mut engine = AudioEngine::default();
        engine.set_muted(true);
        // Мьют и громкость 0 не должны паниковать даже с живым устройством.
        for sound in SoundId::ALL {
            engine.play(sound);
        }
        engine.set_muted(false);
        engine.set_volume(0.0);
        for sound in SoundId::ALL {
            engine.play(sound);
        }
    }

    #[test]
    fn play_without_device_does_not_panic() {
        let mut engine = AudioEngine {
            manager: None,
            muted: false,
            volume: 1.0,
        };
        for sound in SoundId::ALL {
            engine.play(sound);
        }
    }

    #[test]
    fn sound_keys_are_unique() {
        let mut keys: Vec<&str> = SoundId::ALL.iter().map(|sound| sound.key()).collect();
        keys.sort_unstable();
        let count = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), count, "ключи звуков уникальны");
    }
}
