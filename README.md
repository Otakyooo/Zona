# ZONA

Собственный игровой движок (Rust), игра и лаунчер. Сеттинг — Зона в духе братьев
Стругацких («Пикник на обочине»): аномалии, артефакты, экспедиции, исследования.

## Что это

- **Движок с нуля**: `winit` + `wgpu` (Vulkan/DX12) + `rapier3d` + собственный ECS-слой
  на `hecs` с фиксированным тиком симуляции.
- **Игра**: 3D FPS, extraction-рейды (заход со своим снаряжением, добыча выносится,
  смерть = потеря лута), между рейдами — база. PvP + PvE, лут, выживание, прокачка,
  квесты, отрядные сессии 5–10 игроков.
- **Лаунчер**: свой Rust-лаунчер, обновления по `manifest.json` с GitHub Releases
  (zstd-архивы + SHA-256).

## Стек

| Слой | Крейт | Библиотеки |
|---|---|---|
| ECS / шедулер / fixed timestep | `zn-core` | hecs |
| Ввод | `zn-input` | winit |
| Физика | `zn-physics` | rapier3d |
| Рендер | `zn-render` | wgpu |
| Сцена | `zn-scene` | glam, ron |
| Симуляция (общая клиент/сервер) | `zn-sim` | zn-core |
| Сеть | `zn-net` | serde, bincode |
| Клиент | `zn-client` | все движковые крейты |
| Сервер | `zn-server` | zn-sim, zn-net |
| Лаунчер | `zn-launcher` | — |
| Ассеты | `zn-assets` | gltf, image, ktx2 |
| Редактор | `zn-tools-editor` | egui, zn-render, zn-assets |
| Аудио | `zn-audio` | kira |

## Сборка и запуск (Windows)

```powershell
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p zn-client
cargo run -p zn-client -- --content-root game/content --level demo_zone
cargo run -p zn-server -- --port 7700 --max-players 10
cargo run -p zn-launcher -- --channel dev
cargo run -p zn-tools-editor --bin zn-editor
```

Платформа релиза: Windows x64 (wgpu выбирает Vulkan/DX12 автоматически).

## Тулчейн

- Целевой тулчейн для разработки: `stable-x86_64-pc-windows-msvc`
  (`rustup default stable-x86_64-pc-windows-msvc`), как в CI. Запуск клиента —

  ```powershell
  cargo run -p zn-client
  ```

  Требуется MSVC-линкер из **Visual Studio Build Tools** (workload
  «Разработка классических приложений на C++» + Windows SDK):

  ```powershell
  winget install --id Microsoft.VisualStudio.2022.BuildTools `
    --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --add Microsoft.VisualStudio.Component.Windows11SDK.22621 --includeRecommended"
  ```
- Альтернатива — `stable-x86_64-pc-windows-gnu` (rustup):
  `.cargo/config.toml` проекта принудительно переназначает gnu-линкер на
  bundled-gcc тулчейна rustup (`lib/rustlib/x86_64-pc-windows-gnu/bin/self-contained/...`),
  потому что системный WinLibs (Brecht Sanders) gcc, прописанный глобально в
  `~/.cargo/config.toml` этой машины, линкует бинарники с битой таблицей импортов.
  Ранее из-за этого `zn-client` падал на старте с `0xC0000005` в
  `std::sys::pal::windows::init`; ныне разработка идёт на MSVC, где проблемы нет.
- CI (`.github/workflows/ci.yml`) использует стандартный MSVC-тулчейн GitHub-runner.

## Контент

Уровни — RON-файлы `game/content/levels/<имя>.scene.ron`, ассеты —
`game/content/assets/` (glTF/GLB, PNG/JPEG, KTX2-uncompressed). Клиент читает
корень контента из `--content-root` (или `ZN_CONTENT_ROOT`, по умолчанию
`game/content`) и уровень из `--level <имя>`; если файл не найден — предупреждение
и fallback на `demo_zone()`. Редактор: `cargo run -p zn-tools-editor --bin zn-editor` —
New/Open/Save `.scene.ron`, орбитальная камера, пикинг объектов, gizmo
(перемещение/вращение), браузер ассетов с размещением (клик пересекает луч с
землёй y=0 или ближайшим боксом коллайдера), панель свойств (position, scale,
yaw, tint, asset, material, collider, solid). Одноразовая выгрузка демо-уровня:

```powershell
cargo run -p zn-tools-editor --bin zn-editor -- --emit-demo game/content/levels/demo_zone.scene.ron
```

Схема объекта: `position`, `scale`, `yaw`, `color`, `asset` (`builtin://cube/sphere/
cylinder/quad` или относительный путь к glTF), `material` (имя материала внутри
ассета), `collider` (`box`/`cylinder`/`sphere`, по умолчанию box у solid-объектов),
`solid`. Неизвестные поля получают значения по умолчанию (back-compat с RON M1).
Asset-ключи — строки, межкратные ID (`MeshId`/`MaterialId`) живут только внутри
`zn-render`. Загрузка ассетов в клиенте — фоновый поток (std::thread + mpsc);
не загруженное показывается builtin-кубом. GPU-кэши с LRU-выселением по бюджетам
256 МиБ (меши) / 512 МиБ (текстуры). KTX2 поддерживается только в
uncompressed-виде (BasisU не реализован — фолбэк на PNG/JPEG/uncompressed KTX2).

## Дорожная карта

- **M0**: каркас workspace + CI (готово)
- **M1**: вертикальный срез движка (окно, FPS-камера, character controller,
  рендер инстансированной сцены) — готово, валидация: тесты/clippy/fmt зелёные
- **M2**: контент-пайплайн (glTF/KTX2-импорт, CPU-типы + GPU-кэши), редактор
  уровней, стриминг с LRU-кэшами — готово, валидация: тесты/clippy/fmt зелёные,
  клиент запускается из `game/content`
- **M3**: netcode (snapshot/reconciliation), dedicated-сервер, сессии 5–10
- **M4**: геймплей Зоны (аномалии, артефакты, дневник, PvE/PvP, выживание, лут,
  экстракция, прокачка, квесты)
- **M5**: аккаунты, БД, база между рейдами
- **M6**: лаунчер, обновления, инсталлятор

## Управление (M1)

WASD — движение, мышь — обзор, Space — прыжок, Esc — отпустить курсор.
