use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use zn_assets::{import_gltf, ImportedAsset};

/// События фонового загрузчика ассетов.
#[derive(Debug)]
pub enum AssetEvent {
    Loaded { key: String, asset: ImportedAsset },
    Failed { key: String, error: String },
}

/// Фоновый загрузчик ассетов: один worker-поток + очередь ключей + очередь
/// результатов. Импорт glTF/декод текстур идут вне рендер-потока.
pub struct AssetLoader {
    request_tx: Sender<String>,
    event_rx: Receiver<AssetEvent>,
    _worker: thread::JoinHandle<()>,
}

impl AssetLoader {
    pub fn new(content_root: impl Into<PathBuf>) -> Self {
        let content_root = content_root.into();
        let (request_tx, request_rx) = mpsc::channel::<String>();
        let (event_tx, event_rx) = mpsc::channel::<AssetEvent>();
        let worker = thread::spawn(move || {
            while let Ok(key) = request_rx.recv() {
                if key.starts_with("builtin://") {
                    continue; // встроенные примитивы грузить не нужно
                }
                let path = content_root.join(&key);
                log::info!("импорт ассета '{key}' из {}", path.display());
                match import_gltf(&path) {
                    Ok(asset) => {
                        let report = asset.report();
                        log::info!(
                            "ассет '{key}': мешей {}, материалов {}, текстур {}",
                            report.meshes,
                            report.materials,
                            report.textures
                        );
                        if event_tx.send(AssetEvent::Loaded { key, asset }).is_err() {
                            return; // приёмник умер
                        }
                    }
                    Err(error) => {
                        log::warn!("ассет '{key}' не загружен: {error}");
                        if event_tx
                            .send(AssetEvent::Failed {
                                key,
                                error: error.to_string(),
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                }
            }
        });
        Self {
            request_tx,
            event_rx,
            _worker: worker,
        }
    }

    /// Ставит ключ в очередь загрузки (builtin:// игнорируется).
    pub fn request(&self, key: &str) {
        if key.starts_with("builtin://") {
            return;
        }
        if self.request_tx.send(key.to_string()).is_err() {
            log::warn!("загрузчик ассетов остановлен — запрос '{key}' потерян");
        }
    }

    /// Забирает все накопившиеся результаты (неблокирующе).
    pub fn drain_events(&self) -> Vec<AssetEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.event_rx.try_recv() {
            events.push(event);
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("zn-client-assets-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn loader_reports_failed_asset() {
        let root = temp_dir("missing");
        let loader = AssetLoader::new(&root);
        loader.request("assets/teapot.gltf");
        // Ждём ответ воркера с таймаутом.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let events = loader.drain_events();
            if !events.is_empty() {
                match &events[0] {
                    AssetEvent::Failed { key, .. } => {
                        assert_eq!(key, "assets/teapot.gltf");
                    }
                    AssetEvent::Loaded { .. } => panic!("файла не существует — ждали ошибку"),
                }
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("время ожидания события истекло");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn loader_ignores_builtin_keys() {
        let root = temp_dir("builtin");
        let loader = AssetLoader::new(&root);
        loader.request("builtin://cube");
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(loader.drain_events().is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn loader_imports_gltf_from_disk() {
        // Минимальный glTF на диске (data-URI) — воркер должен импортировать.
        let root = temp_dir("gltf");
        let positions: Vec<[f32; 3]> = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let mut buffer = Vec::new();
        for vertex in &positions {
            for component in vertex {
                buffer.extend_from_slice(&component.to_le_bytes());
            }
        }
        let encoded = base64_encode(&buffer);
        let json = format!(
            r#"{{
  "asset": {{ "version": "2.0" }},
  "scene": 0,
  "scenes": [ {{ "nodes": [0] }} ],
  "nodes": [ {{ "mesh": 0 }} ],
  "meshes": [ {{ "primitives": [ {{ "attributes": {{ "POSITION": 0 }}, "mode": 4 }} ] }} ],
  "accessors": [
    {{ "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
      "min": [0,0,0], "max": [1,1,0] }}
  ],
  "bufferViews": [ {{ "buffer": 0, "byteOffset": 0, "byteLength": 36 }} ],
  "buffers": [ {{ "byteLength": 36, "uri": "data:application/octet-stream;base64,{encoded}" }} ]
}}"#
        );
        let assets_dir = root.join("assets");
        std::fs::create_dir_all(&assets_dir).unwrap();
        std::fs::write(assets_dir.join("tri.gltf"), json).unwrap();

        let loader = AssetLoader::new(&root);
        loader.request("assets/tri.gltf");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut imported = false;
        loop {
            for event in loader.drain_events() {
                if let AssetEvent::Loaded { key, asset } = event {
                    assert_eq!(key, "assets/tri.gltf");
                    assert_eq!(asset.meshes.len(), 1);
                    assert_eq!(asset.meshes[0].vertices.len(), 3);
                    imported = true;
                }
            }
            if imported {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("импорт не завершился вовремя");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        std::fs::remove_dir_all(&root).ok();
    }

    fn base64_encode(data: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = *chunk.get(1).unwrap_or(&0) as u32;
            let b2 = *chunk.get(2).unwrap_or(&0) as u32;
            let triple = (b0 << 16) | (b1 << 8) | b2;
            out.push(ALPHABET[(triple >> 18) as usize & 63] as char);
            out.push(ALPHABET[(triple >> 12) as usize & 63] as char);
            if chunk.len() > 1 {
                out.push(ALPHABET[(triple >> 6) as usize & 63] as char);
            } else {
                out.push('=');
            }
            if chunk.len() > 2 {
                out.push(ALPHABET[triple as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
        out
    }
}
