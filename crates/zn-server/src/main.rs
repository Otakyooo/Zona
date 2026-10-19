use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::thread;
use std::time::{Duration, Instant};

use zn_content::ItemCatalog;
use zn_net::{MAX_PLAYERS_PER_SESSION, PROTOCOL_VERSION, SIMULATION_HZ, SNAPSHOT_HZ};
use zn_scene::{LocationId, LocationRegistry, Scene};
use zn_server::{Session, SessionConfig, DEFAULT_PORT, STASH_FILE_DEFAULT};

const USAGE: &str = "usage: zn-server [--bind <addr>] [--port <port>] [--max-players <n>] \
[--content-root <dir>] [--level <name>] [--sim-latency-ms <ms>] [--sim-loss-pct <0..100>] \
[--stash-file <path>|--no-stash]\n\
без --level поднимается мир локаций из <content-root>/locations.ron";

#[derive(Clone, Debug)]
struct ServerArgs {
    bind: String,
    port: u16,
    max_players: usize,
    content_root: PathBuf,
    /// Одиночная сцена вместо мира локаций (разработка и тесты).
    level: Option<String>,
    sim_latency_ms: u64,
    sim_loss_pct: f32,
    /// Файл персиста стэшей профилей (M4d-2). `None` — персист выключен.
    stash_file: Option<PathBuf>,
}

impl Default for ServerArgs {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0".to_string(),
            port: DEFAULT_PORT,
            max_players: MAX_PLAYERS_PER_SESSION,
            content_root: PathBuf::from("game/content"),
            level: None,
            sim_latency_ms: 0,
            sim_loss_pct: 0.0,
            stash_file: Some(PathBuf::from(STASH_FILE_DEFAULT)),
        }
    }
}

fn parse_args() -> Result<ServerArgs, String> {
    let mut args = ServerArgs::default();
    let mut iter = std::env::args().skip(1);
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            // Флаги без значения.
            "--no-stash" => args.stash_file = None,
            // Флаги со значением.
            _ => {
                let value = iter
                    .next()
                    .ok_or_else(|| format!("missing value for {flag}"))?;
                match flag.as_str() {
                    "--bind" => args.bind = value,
                    "--port" => {
                        args.port = value
                            .parse()
                            .map_err(|_| format!("invalid port: {value}"))?
                    }
                    "--max-players" => {
                        args.max_players = value
                            .parse()
                            .map_err(|_| format!("invalid max players: {value}"))?
                    }
                    "--content-root" => args.content_root = PathBuf::from(value),
                    "--level" => args.level = Some(value),
                    "--sim-latency-ms" => {
                        args.sim_latency_ms = value
                            .parse()
                            .map_err(|_| format!("invalid sim latency: {value}"))?
                    }
                    "--sim-loss-pct" => {
                        args.sim_loss_pct = value
                            .parse()
                            .map_err(|_| format!("invalid sim loss pct: {value}"))?
                    }
                    "--stash-file" => args.stash_file = Some(PathBuf::from(value)),
                    other => return Err(format!("unknown flag: {other}")),
                }
            }
        }
    }
    args.max_players = args.max_players.clamp(1, MAX_PLAYERS_PER_SESSION);
    args.sim_loss_pct = args.sim_loss_pct.clamp(0.0, 100.0);
    Ok(args)
}

fn main() {
    env_logger::init();

    let args = match parse_args() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };

    let catalog = load_catalog(&args.content_root);
    let config = SessionConfig {
        max_players: args.max_players,
        out_latency: Duration::from_millis(args.sim_latency_ms),
        out_loss_pct: args.sim_loss_pct,
        stash_file: args.stash_file.clone(),
        ..SessionConfig::default()
    };

    let bind_addr = format!("{}:{}", args.bind, args.port);
    let session_result = if let Some(level) = &args.level {
        let scene = load_scene(&args.content_root, level);
        log::info!("режим одиночной сцены: --level '{level}' (переходы и пузыри мира отключены)");
        Session::bind(&bind_addr, &scene, catalog, config)
    } else {
        let registry = load_registry(&args.content_root);
        let scenes = load_world_scenes(&args.content_root, &registry);
        Session::bind_world(&bind_addr, &registry, scenes, catalog, config)
    };
    let mut session = match session_result {
        Ok(session) => session,
        Err(error) => {
            eprintln!("error: не удалось создать сессию на {bind_addr}: {error}");
            std::process::exit(1);
        }
    };

    let local = session
        .local_addr()
        .map(|addr| addr.to_string())
        .unwrap_or(bind_addr);
    log::info!(
        "ZONA dedicated server: листен {local}, симуляция {SIMULATION_HZ} Гц, снапшоты {SNAPSHOT_HZ} Гц, \
protocol={PROTOCOL_VERSION}, max_players={}, локаций: {}",
        args.max_players,
        session.location_count()
    );
    if args.sim_latency_ms > 0 || args.sim_loss_pct > 0.0 {
        log::warn!(
            "включена диагностика сети: задержка исходящих {} мс, дроп {:.1}%",
            args.sim_latency_ms,
            args.sim_loss_pct
        );
    }

    let (tx, rx) = channel::<String>();
    thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines().map_while(Result::ok) {
            if tx.send(line.trim().to_string()).is_err() {
                break;
            }
        }
    });

    let boot = Instant::now();
    let mut last = Instant::now();
    let mut last_status = Instant::now();
    let mut running = true;

    while running {
        let now = Instant::now();
        let frame_delta = now - last;
        last = now;

        session.poll(frame_delta);

        while let Ok(line) = rx.try_recv() {
            match line.as_str() {
                "quit" | "exit" => running = false,
                "status" | "" => print_status(&mut session, &args, boot),
                "list" => print_clients(&mut session),
                "spawn" => {
                    let id = session.debug_spawn();
                    println!("spawned player {id}");
                }
                other => {
                    if let Some(id) = other.strip_prefix("kick ") {
                        match id.trim().parse::<u64>() {
                            Ok(id) if session.kick(id) => println!("kicked player {id}"),
                            Ok(id) => println!("player {id} not found"),
                            Err(_) => println!("usage: kick <player_id>"),
                        }
                    } else if let Some(rest) = other.strip_prefix("loot ") {
                        print_loot(&mut session, rest);
                    } else if let Some(rest) = other.strip_prefix("tp ") {
                        print_tp(&mut session, rest);
                    } else if other == "bubbles" || other.starts_with("bubbles ") {
                        print_bubbles(&session, other.trim_start_matches("bubbles"));
                    } else {
                        println!(
                            "unknown command: {other} (try: status, list, kick <id>, spawn, \
loot <id> <item> <count>, tp <id> <location>, bubbles [<location>], quit)"
                        );
                    }
                }
            }
        }

        if last_status.elapsed() >= Duration::from_secs(5) {
            last_status = Instant::now();
            print_status(&mut session, &args, boot);
        }

        thread::sleep(Duration::from_millis(2));
    }

    println!("server stopped after {} ticks", session.tick());
    // Персист стэшей (M4d-2): склад профилей переживает остановку сервера.
    session.flush_stashes();
    let _ = std::io::stdout().flush();
}

fn load_scene(content_root: &Path, level: &str) -> Scene {
    let path = content_root
        .join("levels")
        .join(format!("{level}.scene.ron"));
    match Scene::load_ron(&path) {
        Ok(scene) => {
            log::info!(
                "уровень '{level}' загружен из {} ({} объектов)",
                path.display(),
                scene.len()
            );
            scene
        }
        Err(error) => {
            log::warn!(
                "уровень '{level}' не прочитан ({}): {error} — fallback на demo_zone()",
                path.display()
            );
            Scene::demo_zone()
        }
    }
}

fn load_catalog(content_root: &Path) -> ItemCatalog {
    let path = content_root.join("items").join("items.ron");
    match ItemCatalog::load_ron(&path) {
        Ok(catalog) => {
            log::info!(
                "каталог предметов загружен из {} ({} позиций)",
                path.display(),
                catalog.len()
            );
            catalog
        }
        Err(error) => {
            log::warn!(
                "каталог предметов не прочитан ({}): {error} — предметы отключены",
                path.display()
            );
            ItemCatalog::default()
        }
    }
}

/// Реестр локаций `<content_root>/locations.ron`; при ошибке или пустом
/// реестре — встроенный мир.
fn load_registry(content_root: &Path) -> LocationRegistry {
    let path = content_root.join("locations.ron");
    match LocationRegistry::load_ron(&path) {
        Ok(registry) if !registry.locations.is_empty() => {
            log::info!(
                "реестр локаций загружен из {} ({} локаций)",
                path.display(),
                registry.locations.len()
            );
            registry
        }
        Ok(_) => {
            log::warn!("реестр локаций пуст ({}) — встроенный мир", path.display());
            LocationRegistry::builtin()
        }
        Err(error) => {
            log::warn!(
                "реестр локаций не прочитан ({}): {error} — встроенный мир",
                path.display()
            );
            LocationRegistry::builtin()
        }
    }
}

/// Сцены всех локаций реестра; битый или отсутствующий файл заменяется
/// встроенной сценой этой локации.
fn load_world_scenes(content_root: &Path, registry: &LocationRegistry) -> Vec<(LocationId, Scene)> {
    let builtin = Scene::builtin_world();
    let mut scenes = Vec::with_capacity(registry.locations.len());
    for info in &registry.locations {
        let scene = match registry.scene_path(content_root, info.id) {
            Some(path) => match Scene::load_ron(&path) {
                Ok(scene) => {
                    log::info!(
                        "локация '{}': загружена из {} ({} объектов)",
                        info.name,
                        path.display(),
                        scene.len()
                    );
                    scene
                }
                Err(error) => {
                    log::warn!(
                        "локация '{}': {} не прочитана ({error}) — встроенная сцена",
                        info.name,
                        path.display()
                    );
                    builtin_scene(&builtin, info.id)
                }
            },
            None => builtin_scene(&builtin, info.id),
        };
        scenes.push((info.id, scene));
    }
    scenes
}

/// Встроенная сцена по id; нет такой — пустая сцена-заглушка.
fn builtin_scene(builtin: &[(LocationId, Scene)], id: LocationId) -> Scene {
    builtin
        .iter()
        .find(|(builtin_id, _)| *builtin_id == id)
        .map(|(_, scene)| scene.clone())
        .unwrap_or_else(|| Scene::new(format!("location_{id}")))
}

fn print_status(session: &mut Session, args: &ServerArgs, boot: Instant) {
    let stats = session.stats();
    println!(
        "status: uptime={:?} tick={} players={}/{} packets: rx={} tx={} drop={} protocol={} hash={:#018x} items={:#018x}",
        boot.elapsed(),
        session.tick(),
        session.player_count(),
        args.max_players,
        stats.received,
        stats.sent,
        stats.dropped,
        PROTOCOL_VERSION,
        session.content_hash(),
        session.items_hash(),
    );
    let _ = std::io::stdout().flush();
}

/// Консоль `loot <player_id> <item_key> <count>`: выдать предмет игроку.
fn print_loot(session: &mut Session, rest: &str) {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    let [id, key, count] = parts.as_slice() else {
        println!("usage: loot <player_id> <item_key> <count>");
        return;
    };
    let (Ok(id), Ok(count)) = (id.parse::<u64>(), count.parse::<u32>()) else {
        println!("usage: loot <player_id> <item_key> <count>");
        return;
    };
    match session.debug_give(id, key, count) {
        Ok(moved) => println!("gave {moved}/{count} '{key}' to player {id}"),
        Err(error) => println!("loot: {error}"),
    }
    let _ = std::io::stdout().flush();
}

/// Консоль `tp <player_id> <location>`: перенести игрока в точку спавна
/// локации (отладочный телепорт, без переходов).
fn print_tp(session: &mut Session, rest: &str) {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    let [id, location] = parts.as_slice() else {
        println!("usage: tp <player_id> <location>");
        return;
    };
    let (Ok(id), Ok(location)) = (id.parse::<u64>(), location.parse::<LocationId>()) else {
        println!("usage: tp <player_id> <location>");
        return;
    };
    let Some(spawn) = session.simulation_at(location).map(|sim| sim.spawn_point()) else {
        println!("tp: локация {location} не найдена");
        return;
    };
    if session.debug_teleport(id, location, spawn) {
        println!(
            "player {id} teleported to '{}' (location {location})",
            session.location_name(location)
        );
    } else {
        println!("tp: игрок {id} не найден");
    }
    let _ = std::io::stdout().flush();
}

/// Консоль `bubbles [<location>]`: активные пузыри мира (якоря на полу).
fn print_bubbles(session: &Session, rest: &str) {
    let filter = rest.trim().parse::<LocationId>().ok();
    for location in session.location_ids() {
        if filter.is_some_and(|filter| filter != location) {
            continue;
        }
        let bubbles = session.active_bubbles(location);
        println!(
            "  '{}' (id {location}): {} пузырей",
            session.location_name(location),
            bubbles.len()
        );
        for position in bubbles {
            println!("    {position:?}");
        }
    }
    let _ = std::io::stdout().flush();
}

fn print_clients(session: &mut Session) {
    let clients = session.clients();
    if clients.is_empty() {
        println!("клиентов нет");
        return;
    }
    for client in clients {
        println!(
            "  id={} name='{}' addr={} loc='{}' idle={:?}",
            client.player_id, client.name, client.addr, client.location_name, client.idle
        );
    }
    let _ = std::io::stdout().flush();
}
