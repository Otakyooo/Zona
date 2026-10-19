//! Процедурная генерация звуков (A1): короткие моно-WAV (16-бит PCM),
//! синтезируемые в памяти. Бинарные ассеты в репозиторий не добавляем —
//! генерация детерминирована (одинаковый звук → одинаковые байты), поэтому
//! её можно тестировать.

use super::SoundId;

/// Частота дискретизации процедурных звуков.
pub const SAMPLE_RATE: u32 = 22_050;

const TAU: f32 = std::f32::consts::TAU;

/// Байты WAV для звука — детерминированы.
pub fn wav_bytes(sound: SoundId) -> Vec<u8> {
    encode_wav(&synth(sound), SAMPLE_RATE)
}

/// Детерминированный ГПСЧ (xorshift32) → [-1, 1]: шум без внешних crate.
struct Rng(u32);

impl Rng {
    fn next_f32(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

/// Рендер звука: `f` получает время (с), долю длины (0..1) и ГПСЧ.
fn render(duration_s: f32, mut f: impl FnMut(f32, f32, &mut Rng) -> f32) -> Vec<f32> {
    let count = (SAMPLE_RATE as f32 * duration_s) as usize;
    let mut rng = Rng(0x9E37_79B9);
    (0..count)
        .map(|index| {
            let t = index as f32 / SAMPLE_RATE as f32;
            let phase = t / duration_s;
            f(t, phase, &mut rng).clamp(-1.0, 1.0)
        })
        .collect()
}

/// Сэмплы звука — вся генерация живёт здесь.
fn synth(sound: SoundId) -> Vec<f32> {
    match sound {
        // Выстрел: шумовой щелчок с быстрым спадом + низкий удар.
        SoundId::Shot => render(0.30, |t, phase, rng| {
            0.7 * rng.next_f32() * (-14.0 * phase).exp()
                + 0.3 * (TAU * 95.0 * t).sin() * (-8.0 * phase).exp()
        }),
        // Попадание: резкий клик + высокий бlip.
        SoundId::Hit => render(0.10, |t, phase, rng| {
            0.5 * rng.next_f32() * (-30.0 * phase).exp()
                + 0.4 * (TAU * 760.0 * t).sin() * (-25.0 * phase).exp()
        }),
        // Подбор: два восходящих бlipа.
        SoundId::Pickup => render(0.15, |t, phase, _rng| {
            let (freq, start) = if phase < 0.4 {
                (660.0, 0.0)
            } else {
                (990.0, 0.4)
            };
            let local = (phase - start) / 0.6;
            (TAU * freq * t).sin() * (-6.0 * local).exp() * 0.5
        }),
        // Бросок болта: сглаженный шумовой «свист» с колоколообразной огибающей.
        SoundId::Throw => {
            let mut low = 0.0;
            render(0.20, |_t, phase, rng| {
                let noise = rng.next_f32();
                low += 0.25 * (noise - low); // однополюсный ФНЧ
                low * 2.5 * (std::f32::consts::PI * phase).sin()
            })
        }
        // Болт сработал в аномалии: нисходящий «зап» + шумовая вспышка.
        SoundId::BoltAnomaly => render(0.40, |t, phase, rng| {
            let freq = 900.0 * (1.0 - 0.7 * phase);
            0.5 * (TAU * freq * t).sin() * (-4.0 * phase).exp()
                + 0.3 * rng.next_f32() * (-20.0 * phase).exp()
        }),
        // Лечение: мягкий тон с вибрато и плавными краями.
        SoundId::Heal => render(0.45, |t, phase, _rng| {
            let env = (std::f32::consts::PI * phase).sin().powf(0.6);
            let tremolo = 1.0 + 0.15 * (TAU * 9.0 * t).sin();
            (TAU * 520.0 * t).sin() * 0.45 * env * tremolo
        }),
        // Смерть: низкий нисходящий тон и глухой шум.
        SoundId::Death => render(0.80, |t, phase, rng| {
            let freq = 320.0 - 240.0 * phase;
            0.5 * (TAU * freq * t).sin() * (-2.2 * phase).exp()
                + 0.15 * rng.next_f32() * (-3.0 * phase).exp()
        }),
        // Экстракция: двухтоновый «чойс» (для кругов экстракции, M4d-2).
        SoundId::Extract => render(0.70, |t, phase, _rng| {
            (TAU * 587.33 * t).sin() * (-2.5 * phase).exp() * 0.4
                + (TAU * 880.0 * t).sin() * (-1.8 * phase).exp() * 0.3
        }),
        // Панель/интерфейс: короткий клик.
        SoundId::Panel => render(0.06, |t, phase, rng| {
            0.6 * rng.next_f32() * (-25.0 * phase).exp()
                + 0.3 * (TAU * 1400.0 * t).sin() * (-30.0 * phase).exp()
        }),
        // Переход между локациями: восходящий свист.
        SoundId::Transition => render(0.30, |t, phase, rng| {
            let freq = 300.0 + 600.0 * phase;
            0.4 * (TAU * freq * t).sin() * (std::f32::consts::PI * phase).sin()
                + 0.2 * rng.next_f32() * (-6.0 * phase).exp()
        }),
    }
}

/// Упаковка моно-сэмплов в WAV (16-бит PCM).
fn encode_wav(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let data_len = samples.len() * 2;
    let mut bytes = Vec::with_capacity(44 + data_len);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes()); // размер fmt-чанка
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // моно
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // байт/с
    bytes.extend_from_slice(&2u16.to_le_bytes()); // блочное выравнивание
    bytes.extend_from_slice(&16u16.to_le_bytes()); // бит на сэмпл
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data_len as u32).to_le_bytes());
    for sample in samples {
        let amplitude = (*sample * i16::MAX as f32) as i16;
        bytes.extend_from_slice(&amplitude.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_describes_mono_pcm() {
        for sound in SoundId::ALL {
            let bytes = wav_bytes(sound);
            assert!(bytes.len() > 44, "{sound:?}: пустой WAV");
            assert_eq!(&bytes[0..4], b"RIFF", "{sound:?}: нет RIFF");
            assert_eq!(&bytes[8..12], b"WAVE", "{sound:?}: нет WAVE");
            assert_eq!(&bytes[12..16], b"fmt ", "{sound:?}: нет fmt");
            let channels = u16::from_le_bytes([bytes[22], bytes[23]]);
            let bits = u16::from_le_bytes([bytes[34], bytes[35]]);
            let rate = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
            assert_eq!(channels, 1, "{sound:?}: ожидался моно");
            assert_eq!(bits, 16, "{sound:?}: ожидался 16-бит PCM");
            assert_eq!(rate, SAMPLE_RATE, "{sound:?}: частота дискретизации");

            // Заявленная длина данных совпадает с фактическими байтами.
            let declared =
                u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]) as usize;
            assert_eq!(declared, bytes.len() - 44, "{sound:?}: длина data");
            assert!(declared > 2, "{sound:?}: нет сэмплов");
        }
    }

    #[test]
    fn synthesis_is_deterministic_and_sounds_differ() {
        for sound in SoundId::ALL {
            assert_eq!(
                wav_bytes(sound),
                wav_bytes(sound),
                "{sound:?}: недетерминирован"
            );
        }
        // Разные звуки дают разные байты (синтез не выродился в тишину/константу).
        for (index, sound) in SoundId::ALL.iter().enumerate() {
            for other in &SoundId::ALL[index + 1..] {
                assert_ne!(
                    wav_bytes(*sound),
                    wav_bytes(*other),
                    "{sound:?} == {other:?}"
                );
            }
        }
        // Ни один звук не «немой»: где-то в файле есть ненулевые сэмплы.
        for sound in SoundId::ALL {
            let bytes = wav_bytes(sound);
            let loud = bytes[44..].iter().filter(|byte| **byte != 0).count();
            assert!(loud > (bytes.len() - 44) / 10, "{sound:?}: почти тишина");
        }
    }

    #[test]
    fn wav_body_matches_synth_and_has_signal() {
        for sound in SoundId::ALL {
            let bytes = wav_bytes(sound);
            let (pairs, _rest) = bytes[44..].as_chunks::<2>();
            assert_eq!(pairs.len(), synth(sound).len(), "{sound:?}: число сэмплов");
            let peak = pairs
                .iter()
                .map(|pair| i16::from_le_bytes(*pair).unsigned_abs())
                .max()
                .unwrap_or(0);
            assert!(peak > 0, "{sound:?}: нет сигнала");
        }
    }
}
