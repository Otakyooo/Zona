use kira::{AudioManager, AudioManagerSettings, DefaultBackend};

#[derive(Default)]
pub struct AudioEngine {
    manager: Option<AudioManager<DefaultBackend>>,
}

impl AudioEngine {
    pub fn new() -> Self {
        Self::default().initialized()
    }

    pub fn initialized(mut self) -> Self {
        match AudioManager::<DefaultBackend>::new(AudioManagerSettings::default()) {
            Ok(manager) => {
                log::info!("audio engine initialized");
                self.manager = Some(manager);
            }
            Err(error) => {
                log::warn!("audio device unavailable ({error}); running muted");
            }
        }
        self
    }

    pub fn enabled(&self) -> bool {
        self.manager.is_some()
    }
}

impl std::fmt::Debug for AudioEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioEngine").field("enabled", &self.enabled()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::AudioEngine;

    #[test]
    fn engine_initializes_or_fails_softly() {
        let engine = AudioEngine::new();
        println!("audio enabled: {}", engine.enabled());
    }
}
