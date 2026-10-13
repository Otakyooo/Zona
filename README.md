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
| Ассеты | `zn-assets` | gltf, image (M2) |
| Аудио | `zn-audio` | kira |

## Сборка и запуск (Windows)

```powershell
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p zn-client
cargo run -p zn-server -- --port 7700 --max-players 10
cargo run -p zn-launcher -- --channel dev
```

Платформа релиза: Windows x64 (wgpu выбирает Vulkan/DX12 автоматически).

## Дорожная карта

- **M0**: каркас workspace + CI (готово)
- **M1**: вертикальный срез движка (окно, FPS-камера, character controller,
  рендер инстансированной сцены) — в работе
- **M2**: контент-пайплайн (glTF), редактор уровней, стриминг
- **M3**: netcode (snapshot/reconciliation), dedicated-сервер, сессии 5–10
- **M4**: геймплей Зоны (аномалии, артефакты, дневник, PvE/PvP, выживание, лут,
  экстракция, прокачка, квесты)
- **M5**: аккаунты, БД, база между рейдами
- **M6**: лаунчер, обновления, инсталлятор

## Управление (M1)

WASD — движение, мышь — обзор, Space — прыжок, Esc — отпустить курсор.
