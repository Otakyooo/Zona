# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

ZONA — собственный игровой движок на Rust (winit + wgpu + rapier3d + hecs), extraction-FPS в сеттинге Стругацких, dedicated-сервер и лаунчер. Документация, коммиты и комментарии — на русском. Подробности по каждой подсистеме — в `README.md` (разделы по милстоунам M1–M6, M4g, A1); его нужно обновлять вместе с изменениями поведения.

## Команды

```powershell
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings   # CI-гейт на каждый пуш (ubuntu)
cargo test --workspace                                   # CI только на PR/вручную (windows)
cargo test -p zn-sim                                     # тесты одного крейта
cargo test -p zn-scene content_hash                      # тесты по подстроке имени
cargo test -p zn-sim --test sim <имя_теста>              # конкретный интеграционный тест

cargo run -p zn-client                                   # offline, demo_zone
cargo run -p zn-client -- --level kordon                 # offline, одна сцена
cargo run -p zn-server -- --port 7700                    # мир локаций
cargo run -p zn-client -- --server 127.0.0.1:7700        # клиент к локальному серверу
cargo run -p zn-server -- --no-db                        # без SQLite-персиста
cargo run -p zn-launcher [-- --cli]                      # dev-режим: клиент из target/
cargo run -p zn-tools-editor --bin zn-editor             # редактор уровней
cargo run -p zn-tools-editor --bin zn-editor -- --emit-world game/content   # перегенерировать мир из кода
```

Готовность изменения в этом проекте = зелёные fmt + clippy (`-D warnings`) + тесты; для сетевых правок — ручной smoke «сервер + 2 клиента».

Тулчейн: Windows, `stable-x86_64-pc-windows-msvc`, MSRV 1.95. `.cargo/config.toml` переопределяет линкер только для `-gnu` таргета (системный WinLibs gcc на этой машине даёт битые бинарники) — не удалять.

## Архитектура

Workspace: движковые крейты в `crates/`, игровые — в `game/` (`zn-sim`, `zn-content`), контент — `game/content/`.

- **`zn-sim`** (`game/zn-sim`) — общая детерминированная симуляция 60 Гц, которую исполняют и сервер (авторитетно), и клиент (prediction). Вся геймплейная логика (движение, бой, лут, аномалии, экстракция, выбросы) живёт здесь, чтобы клиентское предсказание совпадало с сервером. Интеграционные тесты — `game/zn-sim/tests/sim.rs`.
- **`zn-net`** — протокол UDP + bincode, одно сообщение на датаграмму, бюджет 1400 Б без фрагментации. `PROTOCOL_VERSION` в `crates/zn-net/src/lib.rs` — поднимать при любом изменении формата сообщений (handshake отклоняет несовпадение).
- **`zn-server`** — `lib.rs` (матч, мир локаций: одна симуляция на локацию, консоль, аккаунты) + `db.rs` (SQLite/WAL: accounts, stashes, wallet, facilities; миграция легаси `stashes.ron`). Снапшоты 30 Гц.
- **`zn-client`** — `app.rs` (основной цикл, HUD, панели, ввод), `net.rs`, `prediction.rs` (reconcile: откат → реплей неподтверждённых инпутов), `interp.rs` (удалённые игроки на 100 мс назад), `assets.rs` (фоновая загрузка ассетов).
- **`zn-scene`** — формат сцены `*.scene.ron`, `LocationRegistry`, `SceneEnvironment`, `content_hash`. Мир **продублирован в коде** (`LocationRegistry::builtin`, `Scene::builtin_world`) как fallback к файлам `game/content/locations.ron` и `levels/*.scene.ron`: клиент и сервер сверяют `content_hash` в handshake, поэтому при правке builtin-мира нужно перегенерировать файлы через `--emit-world` (и наоборот), иначе тесты `builtin_world_matches_registry` и коннект ломаются. Новые поля сцены — с `#[serde(default)]` (back-compat старых RON).
- **`zn-content`** — каталог предметов `game/content/items/items.ron`; сетевой ID предмета = индекс в каталоге, отсортированном по `key`, хэш каталога тоже едет в handshake.
- **`zn-render`** — wgpu-рендер: HDR offscreen + пост (bloom/виньетка/ACES), тени, GPU-кэши с LRU. `MeshId`/`MaterialId` не выходят за пределы `zn-render`; снаружи ассеты адресуются строковыми ключами (`builtin://…` или путь к glTF).
- **`zn-assets`** — glTF/PNG/KTX2/BasisU импорт и процедурные встроенные текстуры. **`zn-physics`** — rapier, прокси-коллайдеры из сцены. **`zn-core`** — ECS/fixed timestep. **`zn-audio`** — kira, процедурные звуки; без звукового устройства обязан работать молча (CI).
- **`zn-launcher`** — eframe GUI + CLI, обновление по `manifest.json` из GitHub Releases (SHA-256, атомарный свап, самообновление). TLS — системный (native-tls), не rustls: rustls режется DPI у тестеров.
- **`zn-tools-editor`** — редактор уровней на egui-wgpu (undo/redo снимками, gizmo, атмосфера).

## Релизы и деплой

Пуш в `main` → `.github/workflows/release.yml` собирает клиент (Windows) и сервер (Linux) в rolling prerelease «dev» с `manifest.json`. Сервер на `192.168.0.47` подтягивает сборки сам (`deploy/zona-update.sh`, pull-модель с откатом); на том же боксе живёт anicast — его сервисы не трогать. Скрипты в `scripts/` (`publish-launcher-update.ps1`, `update-server-addr.ps1`) публикуют в релиз напрямую — это внешние действия.

`.kilo/plans/` и `.workbuddy-ai/` — локальные заметки других ИИ-инструментов (в git не трекаются); планы милстоунов в `.kilo/plans/` полезны как контекст.
