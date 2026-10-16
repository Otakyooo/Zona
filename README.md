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
cargo run -p zn-client -- --server 127.0.0.1:7700
cargo run -p zn-server -- --port 7700 --max-players 10
cargo run -p zn-server -- --sim-latency-ms 120 --sim-loss-pct 10
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

## Сеть (M3)

Dedicated-сервер и клиент общаются по UDP (bincode, протокол v2; одна датаграмма —
одно сообщение; лимит 1200 Б, фрагментация не реализована). Один процесс сервера —
один матч, до 10 игроков.

- **Клиент**: `--server host:port`. Handshake `ClientHello` (версия протокола +
  `content_hash` уровня + имя) с повтором каждые 500 мс; `ServerReject`
  (версия/контент/нет слотов) → offline. Дальше — `InputPacket` (последние 4
  инпута: потеря датаграммы не превращается в потерю движения), `Ping` раз в
  секунду (RTT — в заголовке окна), `ClientBye` при выходе.
- **Симуляция** 60 Гц — общая кинематика `zn-sim` у клиента и сервера; снапшоты
  30 Гц с персональным `acked_input_tick`. Клиент предсказывает движение локально
  и сверяет со снапшотом: reconcile = восстановление состояния → реплей
  неучтённых инпутов → сравнение; небольшие поправки гаснут визуально (~0.15 с),
  большие (> 2 м) — мгновенный снап.
- **Удалённые игроки** рисуются капсулами с интерполяцией (рендер на 100 мс
  назад, пара снапшотов вокруг тика, yaw по кратчайшей дуге); цвет детерминирован
  по `player_id`. Себя клиент не рисует.
- **Таймауты**: сервер забывает клиента после 5 с тишины; клиент уходит в offline
  после 3 с без снапшотов (заголовок окна: «нет связи»). Реконнекта нет — перезапуск.
- **Ограничения M3**: нет фрагментации (≤ 1200 Б), игроки не сталкиваются друг с
  другом, сервер авторитетен (античит вне рамок M3), M3 — только сетевое движение
  без геймплея.
- **Консоль сервера**: `status`, `list`, `kick <id>`, `spawn` (отладочный игрок),
  `quit`. Флаги `--sim-latency-ms`/`--sim-loss-pct` имитируют задержку и потери
  исходящих (для ручной проверки, не для CI).
- **Smoke**: `cargo run -p zn-server` + два `cargo run -p zn-client -- --server
  127.0.0.1:7700`; выход одного клиента убирает его капсулу у второго, kick с
  консоли — то же самое.

## Дорожная карта

- **M0**: каркас workspace + CI (готово)
- **M1**: вертикальный срез движка (окно, FPS-камера, character controller,
  рендер инстансированной сцены) — готово, валидация: тесты/clippy/fmt зелёные
- **M2**: контент-пайплайн (glTF/KTX2-импорт, CPU-типы + GPU-кэши), редактор
  уровней, стриминг с LRU-кэшами — готово, валидация: тесты/clippy/fmt зелёные,
  клиент запускается из `game/content`
- **M3**: netcode (клиентский prediction/reconciliation, интерполяция удалённых
  игроков), dedicated-сервер (1 матч, ≤ 10 игроков), UDP-протокол v2 — готово,
  валидация: тесты/clippy/fmt зелёные, smoke: сервер + 2 клиента
- **M4**: геймплей Зоны (аномалии, артефакты, дневник, PvE/PvP, выживание, лут,
  экстракция, прокачка, квесты)
- **M5**: аккаунты, БД, база между рейдами
- **M6**: лаунчер, обновления, инсталлятор

## Управление (M1–M3)

WASD — движение, мышь — обзор, Space — прыжок, Esc — отпустить курсор.
В сетевой сессии заголовок окна показывает статус: `подключение…` / `ping N ms` /
`нет связи` (после перехода в offline).
