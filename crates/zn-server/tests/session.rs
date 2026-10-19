//! E2E-тесты UDP-сессии на loopback: сервер и «тупые» клиенты через `zn-net`.
//! Время сессии виртуальное (`Session::poll(dt)`), wall-clock-ожиданий нет —
//! только короткие `yield_now` для доставки loopback-датаграмм.

use std::net::{SocketAddr, UdpSocket};
use std::sync::OnceLock;
use std::time::Duration;

use zn_net::{
    decode_message, encode_message, quantize_pos, ClientInput, GameEvent, InventoryAction, Message,
    PlayerNetState, RejectReason, StashAction, FLAG_FIRE, FLAG_GROUNDED, PROTOCOL_VERSION,
};
use zn_scene::{
    world_content_hash, LocationId, LocationRegistry, Scene, LOCATION_BASE, LOCATION_KORDON,
    LOCATION_SVALKA,
};
use zn_server::{Session, SessionConfig};
use zn_sim::{crate_container_id, Simulation, Stack, Vec3};

struct TestClient {
    socket: UdpSocket,
}

impl TestClient {
    fn connect(server: SocketAddr) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.connect(server).unwrap();
        socket.set_nonblocking(true).unwrap();
        Self { socket }
    }

    fn send(&self, message: &Message) {
        let bytes = encode_message(message).unwrap();
        self.socket.send(&bytes).unwrap();
    }

    fn hello(&self, content_hash: u64) {
        self.hello_profile(content_hash, None);
    }

    /// Hello с профилем (M4d-2): ключ персистентного склада.
    fn hello_profile(&self, content_hash: u64, profile: Option<&str>) {
        self.send(&Message::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            content_hash,
            items_hash: test_items_hash(),
            name: "Тестер".to_string(),
            profile: profile.map(str::to_string),
        });
    }

    /// Обыск/подбор (E) без redundancy — тесты сами контролируют повторы.
    fn interact(&self, tick: u64) {
        self.send(&Message::Interact { tick });
    }

    fn inventory_action(&self, tick: u64, action: InventoryAction) {
        self.send(&Message::InventoryAction { tick, action });
    }

    fn stash_action(&self, tick: u64, action: StashAction) {
        self.send(&Message::StashAction { tick, action });
    }

    fn try_recv(&self) -> Option<Message> {
        let mut buf = [0u8; 2048];
        match self.socket.recv(&mut buf) {
            Ok(len) => Some(decode_message(&buf[..len]).unwrap()),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => None,
            Err(error) => panic!("recv: {error}"),
        }
    }

    /// Последний снапшот из накопленных (остальные отбрасываются).
    fn last_snapshot(&self) -> Option<(u64, u64, Vec<PlayerNetState>)> {
        let mut last = None;
        while let Some(message) = self.try_recv() {
            if let Message::Snapshot {
                server_tick,
                acked_input_tick,
                players,
                ..
            } = message
            {
                last = Some((server_tick, acked_input_tick, players));
            }
        }
        last
    }
}

/// Каталог предметов из контента репозитория (cwd теста — crate root).
fn test_catalog() -> zn_content::ItemCatalog {
    zn_content::ItemCatalog::load_ron("../../game/content/items/items.ron")
        .expect("каталог предметов")
}

/// Хэш каталога — один на весь тестовый процесс.
fn test_items_hash() -> u64 {
    static HASH: OnceLock<u64> = OnceLock::new();
    *HASH.get_or_init(|| test_catalog().content_hash())
}

fn test_session(max_players: usize) -> (Session, SocketAddr) {
    let scene = Scene::demo_zone();
    let config = SessionConfig {
        max_players,
        ..SessionConfig::default()
    };
    let session = Session::bind("127.0.0.1:0", &scene, test_catalog(), config).unwrap();
    let addr = session.local_addr().unwrap();
    (session, addr)
}

/// Сессия мира локаций (как `zn-server` без `--level`).
fn test_world_session(max_players: usize) -> (Session, SocketAddr) {
    let registry = LocationRegistry::builtin();
    let scenes = Scene::builtin_world();
    let config = SessionConfig {
        max_players,
        ..SessionConfig::default()
    };
    let session =
        Session::bind_world("127.0.0.1:0", &registry, scenes, test_catalog(), config).unwrap();
    let addr = session.local_addr().unwrap();
    (session, addr)
}

/// Хэш мира — тот же, что клиент считает в handshake.
fn test_world_hash() -> u64 {
    let registry = LocationRegistry::builtin();
    let scenes = Scene::builtin_world();
    let refs: Vec<(LocationId, &Scene)> = scenes.iter().map(|(id, scene)| (*id, scene)).collect();
    world_content_hash(&registry, &refs)
}

/// Крутит виртуальное время сессии (по 33 мс), пока условие не выполнится.
fn poll_until(
    session: &mut Session,
    rounds: usize,
    mut done: impl FnMut(&Session) -> bool,
) -> bool {
    for _ in 0..rounds {
        session.poll(Duration::from_millis(33));
        if done(session) {
            return true;
        }
        std::thread::yield_now();
    }
    false
}

/// Крутит виртуальное время сессии (по 1 мс), пока клиент не получит
/// сообщение, удовлетворяющее предикату. Верхняя граница — 2 с виртуального
/// времени (меньше `SERVER_TIMEOUT`).
fn expect_message(
    session: &mut Session,
    client: &TestClient,
    mut matches: impl FnMut(&Message) -> bool,
) -> Message {
    for _ in 0..2000 {
        session.poll(Duration::from_millis(1));
        while let Some(message) = client.try_recv() {
            if matches(&message) {
                return message;
            }
        }
        std::thread::yield_now();
    }
    panic!("сообщение не получено за 2000 итераций");
}

fn expect_welcome(session: &mut Session, client: &TestClient) -> u64 {
    match expect_message(session, client, |message| {
        matches!(message, Message::ServerWelcome { .. })
    }) {
        Message::ServerWelcome { player_id, .. } => player_id,
        other => unreachable!("предикат гарантирует Welcome: {other:?}"),
    }
}

fn find_player(players: &[PlayerNetState], player_id: u64) -> PlayerNetState {
    *players
        .iter()
        .find(|player| player.player_id == player_id)
        .unwrap_or_else(|| panic!("нет игрока {player_id} в снапшоте"))
}

#[test]
fn handshake_yields_welcome_with_scene_spawn() {
    let (mut session, addr) = test_session(10);
    let client = TestClient::connect(addr);
    let scene = Scene::demo_zone();

    client.hello(scene.content_hash());
    let reply = expect_message(&mut session, &client, |message| {
        matches!(
            message,
            Message::ServerWelcome { .. } | Message::ServerReject { .. }
        )
    });

    match reply {
        Message::ServerWelcome {
            player_id, spawn, ..
        } => {
            assert_eq!(player_id, 1);
            assert_eq!(spawn, scene.spawn.to_array());
        }
        other => panic!("ожидали Welcome, получили {other:?}"),
    }
    assert_eq!(session.player_count(), 1);
    let clients = session.clients();
    assert_eq!(clients.len(), 1);
    assert_eq!(clients[0].name, "Тестер");
}

#[test]
fn repeated_hello_resends_welcome_without_second_player() {
    let (mut session, addr) = test_session(10);
    let client = TestClient::connect(addr);
    client.hello(Scene::demo_zone().content_hash());
    let first = expect_welcome(&mut session, &client);

    // Welcome мог потеряться — клиент повторяет Hello; получает тот же id.
    client.hello(Scene::demo_zone().content_hash());
    let second = expect_welcome(&mut session, &client);

    assert_eq!(first, second);
    assert_eq!(session.player_count(), 1);
}

#[test]
fn wrong_protocol_version_is_rejected() {
    let (mut session, addr) = test_session(10);
    let client = TestClient::connect(addr);

    client.send(&Message::ClientHello {
        protocol_version: PROTOCOL_VERSION + 1,
        content_hash: Scene::demo_zone().content_hash(),
        items_hash: test_items_hash(),
        name: "Чужак".to_string(),
        profile: None,
    });
    let reply = expect_message(&mut session, &client, |message| {
        matches!(message, Message::ServerReject { .. })
    });

    assert_eq!(
        reply,
        Message::ServerReject {
            reason: RejectReason::VersionMismatch {
                server: PROTOCOL_VERSION
            }
        }
    );
    assert_eq!(session.player_count(), 0);
}

#[test]
fn wrong_content_hash_is_rejected() {
    let (mut session, addr) = test_session(10);
    let client = TestClient::connect(addr);

    client.hello(Scene::demo_zone().content_hash() ^ 0xFFFF);
    let reply = expect_message(&mut session, &client, |message| {
        matches!(message, Message::ServerReject { .. })
    });

    assert_eq!(
        reply,
        Message::ServerReject {
            reason: RejectReason::ContentMismatch
        }
    );
    assert_eq!(session.player_count(), 0);
}

#[test]
fn wrong_items_hash_is_rejected() {
    let (mut session, addr) = test_session(10);
    let client = TestClient::connect(addr);

    client.send(&Message::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        content_hash: Scene::demo_zone().content_hash(),
        items_hash: test_items_hash() ^ 0xFFFF,
        name: "Чужак".to_string(),
        profile: None,
    });
    let reply = expect_message(&mut session, &client, |message| {
        matches!(message, Message::ServerReject { .. })
    });

    assert_eq!(
        reply,
        Message::ServerReject {
            reason: RejectReason::ContentMismatch
        }
    );
    assert_eq!(session.player_count(), 0);
}

#[test]
fn session_full_rejects_second_client() {
    let (mut session, addr) = test_session(1);
    let first = TestClient::connect(addr);
    let second = TestClient::connect(addr);
    let hash = Scene::demo_zone().content_hash();

    first.hello(hash);
    expect_welcome(&mut session, &first);

    second.hello(hash);
    let reply = expect_message(&mut session, &second, |message| {
        matches!(message, Message::ServerReject { .. })
    });

    assert_eq!(
        reply,
        Message::ServerReject {
            reason: RejectReason::SessionFull
        }
    );
    assert_eq!(session.player_count(), 1);
}

#[test]
fn inputs_are_acked_and_move_the_player() {
    let (mut session, addr) = test_session(10);
    let client = TestClient::connect(addr);
    let scene = Scene::demo_zone();
    client.hello(scene.content_hash());
    let player_id = expect_welcome(&mut session, &client);

    let spawn = scene.spawn.to_array();
    let mut next_tick = 0u64;
    let mut last_position = spawn;
    let mut last_ack = 0u64;
    let mut grounded = false;

    for _ in 0..60 {
        let inputs: Vec<ClientInput> = (0..2)
            .map(|_| {
                let mut input = ClientInput::new(next_tick);
                input.move_forward = 1.0;
                next_tick += 1;
                input
            })
            .collect();
        client.send(&Message::InputPacket { inputs });
        session.poll(Duration::from_millis(33));
        if let Some((_, ack, players)) = client.last_snapshot() {
            last_ack = last_ack.max(ack);
            let me = find_player(&players, player_id);
            last_position = me.position;
            grounded = me.flags & FLAG_GROUNDED != 0;
        }
        std::thread::yield_now();

        if last_ack > 0 && grounded && (last_position[0] - spawn[0]).abs() > 1.0 {
            break;
        }
    }

    assert!(
        last_ack > 0,
        "ack снапшота должен расти с обработанными инпутами"
    );
    assert!(grounded, "игрок должен приземлиться на пол");
    // Спавн смотрит вдоль -X (yaw = π): W двигает в -X.
    assert!(
        last_position[0] < spawn[0] - 1.0,
        "игрок не сдвинулся: {last_position:?}"
    );
    // Верх пола demo_zone — y = -0.25 (пол 0.5 м от центра -0.5).
    assert!(
        (last_position[1] - -0.25).abs() < 0.1,
        "игрок должен стоять на полу: {last_position:?}"
    );
}

#[test]
fn ping_gets_pong_with_server_tick() {
    let (mut session, addr) = test_session(10);
    let client = TestClient::connect(addr);
    client.hello(Scene::demo_zone().content_hash());
    expect_welcome(&mut session, &client);

    client.send(&Message::Ping { nonce: 42 });
    let reply = expect_message(&mut session, &client, |message| {
        matches!(message, Message::Pong { .. })
    });

    match reply {
        Message::Pong { nonce, server_tick } => {
            assert_eq!(nonce, 42);
            assert!(server_tick <= session.tick());
        }
        other => unreachable!("предикат гарантирует Pong: {other:?}"),
    }
}

#[test]
fn silent_client_is_timed_out_on_virtual_time() {
    let (mut session, addr) = test_session(10);
    let client = TestClient::connect(addr);
    client.hello(Scene::demo_zone().content_hash());
    expect_welcome(&mut session, &client);
    assert_eq!(session.player_count(), 1);

    // 5.2 с виртуального времени молчания — больше SERVER_TIMEOUT (5 с).
    for _ in 0..520 {
        session.poll(Duration::from_millis(10));
        let _ = client.try_recv();
    }

    assert_eq!(session.player_count(), 0);
    assert!(session.clients().is_empty());
}

#[test]
fn client_bye_removes_player_immediately() {
    let (mut session, addr) = test_session(10);
    let client = TestClient::connect(addr);
    client.hello(Scene::demo_zone().content_hash());
    expect_welcome(&mut session, &client);
    assert_eq!(session.player_count(), 1);

    client.send(&Message::ClientBye);
    session.poll(Duration::from_millis(1));

    assert_eq!(session.player_count(), 0);
    assert!(session.clients().is_empty());
}

#[test]
fn snapshots_contain_both_players_and_positions_converge() {
    let (mut session, addr) = test_session(10);
    let client_a = TestClient::connect(addr);
    let client_b = TestClient::connect(addr);
    let scene = Scene::demo_zone();
    let hash = scene.content_hash();

    client_a.hello(hash);
    let player_a = expect_welcome(&mut session, &client_a);
    client_b.hello(hash);
    let player_b = expect_welcome(&mut session, &client_b);
    assert_ne!(player_a, player_b);

    let spawn = scene.spawn.to_array();
    let mut next_tick = 0u64;
    let mut snapshot_a = None;
    let mut snapshot_b = None;

    for _ in 0..80 {
        let mut inputs_a = Vec::new();
        let mut inputs_b = Vec::new();
        for _ in 0..2 {
            let mut forward = ClientInput::new(next_tick);
            forward.move_forward = 1.0; // A: вперёд (−X)
            inputs_a.push(forward);

            let mut strafe = ClientInput::new(next_tick);
            strafe.move_right = 1.0; // B: вправо (−Z при yaw = π)
            inputs_b.push(strafe);

            next_tick += 1;
        }
        client_a.send(&Message::InputPacket { inputs: inputs_a });
        client_b.send(&Message::InputPacket { inputs: inputs_b });
        session.poll(Duration::from_millis(33));
        snapshot_a = client_a.last_snapshot().or(snapshot_a);
        snapshot_b = client_b.last_snapshot().or(snapshot_b);
        std::thread::yield_now();

        let moved = |snapshot: &Option<(u64, u64, Vec<PlayerNetState>)>| {
            snapshot.as_ref().is_some_and(|(_, ack, players)| {
                *ack > 0 && (find_player(players, player_a).position[0] - spawn[0]).abs() > 1.0
            })
        };
        if moved(&snapshot_a) && moved(&snapshot_b) {
            break;
        }
    }

    let (_, ack_a, players_a) = snapshot_a.expect("A должен получить снапшот");
    let (_, ack_b, players_b) = snapshot_b.expect("B должен получить снапшот");
    assert!(ack_a > 0 && ack_b > 0, "ack обоих клиентов должен расти");
    assert_eq!(players_a.len(), 2, "снапшот A содержит обоих игроков");
    assert_eq!(players_b.len(), 2, "снапшот B содержит обоих игроков");

    // Направления движения: A — вперёд (−X), B — вправо (−Z).
    let a_from_a = find_player(&players_a, player_a);
    let b_from_a = find_player(&players_a, player_b);
    let a_from_b = find_player(&players_b, player_a);
    let b_from_b = find_player(&players_b, player_b);
    assert!(
        a_from_a.position[0] < spawn[0] - 1.0,
        "A не сдвинулся: {a_from_a:?}"
    );
    assert!(
        b_from_b.position[2] < spawn[2] - 1.0,
        "B не сдвинулся: {b_from_b:?}"
    );

    // Обоим клиентам разослан один и тот же тик состояния.
    for (left, right) in [
        (a_from_a.position, a_from_b.position),
        (b_from_a.position, b_from_b.position),
    ] {
        let delta =
            (left[0] - right[0]).abs() + (left[1] - right[1]).abs() + (left[2] - right[2]).abs();
        assert!(delta < 0.5, "позиции разошлись: {left:?} vs {right:?}");
    }
}

#[test]
fn shooting_replicates_events_and_named_death() {
    let (mut session, addr) = test_session(10);
    let shooter = TestClient::connect(addr);
    let victim = TestClient::connect(addr);
    let hash = Scene::demo_zone().content_hash();

    shooter.hello(hash);
    let shooter_id = expect_welcome(&mut session, &shooter);
    victim.hello(hash);
    let victim_id = expect_welcome(&mut session, &victim);
    assert_ne!(shooter_id, victim_id);
    // Патроны стрелку (M4d-3a): без них выстрелов не будет.
    assert_eq!(session.debug_give(shooter_id, "ammo_545", 60).unwrap(), 60);

    // Оба стоят на спавне уровня (капсулы в одной точке), стрелок удерживает
    // огонь. Выстрел из глаз проходит «сквозь» капсулу жертвы и попадает в
    // зону головы — двух попаданий достаточно.
    let mut next_tick = 0u64;
    let mut saw_shot = false;
    let mut death: Option<(u64, u64, String, String)> = None;

    for _ in 0..30 {
        let inputs: Vec<ClientInput> = (0..2)
            .map(|_| {
                let mut input = ClientInput::new(next_tick);
                input.buttons = FLAG_FIRE;
                next_tick += 1;
                input
            })
            .collect();
        shooter.send(&Message::InputPacket { inputs });
        session.poll(Duration::from_millis(33));

        while let Some(message) = shooter.try_recv() {
            if let Message::Snapshot { events, .. } = message {
                for event in events {
                    match event {
                        GameEvent::Shot { shooter: id, .. } if id == shooter_id => {
                            saw_shot = true;
                        }
                        GameEvent::Death {
                            victim,
                            victim_name,
                            killer,
                            killer_name,
                        } => death = Some((victim, killer, victim_name, killer_name)),
                        _ => {}
                    }
                }
            }
        }
        std::thread::yield_now();
        if death.is_some() {
            break;
        }
    }

    assert!(saw_shot, "события Shot должны доезжать в снапшотах");
    let (seen_victim, seen_killer, victim_name, killer_name) =
        death.expect("цель должна погибнуть от попаданий");
    assert_eq!(seen_victim, victim_id);
    assert_eq!(seen_killer, shooter_id);
    // Имена подставляет серверная сессия (знает таблицу клиентов).
    assert_eq!(victim_name, "Тестер");
    assert_eq!(killer_name, "Тестер");
}

// ── Лут (M4b) ───────────────────────────────────────────────────────────

/// Индекс ящика demo_zone у точки спавна (ящик «цепочки» M4b, смещён от
/// коридора движения спавна — вдоль +Z).
fn spawn_crate_index(scene: &Scene) -> usize {
    scene
        .objects
        .iter()
        .position(|object| {
            object.loot.is_some()
                && (object.position.x + 1.3).abs() < 0.01
                && (object.position.z - 15.0).abs() < 0.01
        })
        .expect("в demo_zone есть ящик у спавна")
}

#[test]
fn interact_opens_crate_take_all_and_dedupes_ticks() {
    let (mut session, addr) = test_session(10);
    let client = TestClient::connect(addr);
    let scene = Scene::demo_zone();
    client.hello(scene.content_hash());
    expect_welcome(&mut session, &client);

    let container = crate_container_id(u32::try_from(spawn_crate_index(&scene)).unwrap());
    let catalog = test_catalog();
    let ammo = catalog.index_of("ammo_545").unwrap();
    let bolt = catalog.index_of("bolt").unwrap();

    // Спавн смотрит строго на ящик: одно нажатие E открывает панель.
    client.interact(1);
    let update = expect_message(
        &mut session,
        &client,
        |message| matches!(message, Message::ContainerUpdate { container: c, .. } if *c == container),
    );
    let Message::ContainerUpdate { items, .. } = update else {
        unreachable!("предикат гарантирует ContainerUpdate")
    };
    assert_eq!(items.len(), 2, "патроны + болты");
    assert!(items
        .iter()
        .any(|stack| stack.item == ammo && stack.count == 60));
    assert!(items
        .iter()
        .any(|stack| stack.item == bolt && stack.count == 10));

    // Повтор с тем же tick панель не закрывает (дедупликация сервера).
    client.interact(1);
    for _ in 0..20 {
        session.poll(Duration::from_millis(1));
    }
    let mut close_seen = false;
    while let Some(message) = client.try_recv() {
        if matches!(message, Message::ContainerUpdate { container: 0, .. }) {
            close_seen = true;
        }
    }
    assert!(!close_seen, "повтор с тем же tick должен игнорироваться");

    // Взять всё: обе стопки влезают в пустой инвентарь.
    client.inventory_action(2, InventoryAction::TakeAll { container });
    let update = expect_message(&mut session, &client, |message| {
        matches!(message, Message::InventoryUpdate { slots }
            if slots.iter().flatten().any(|stack| stack.item == ammo && stack.count == 60))
    });
    let Message::InventoryUpdate { slots } = update else {
        unreachable!("предикат гарантирует InventoryUpdate")
    };
    assert!(slots
        .iter()
        .flatten()
        .any(|stack| stack.item == bolt && stack.count == 10));
    expect_message(&mut session, &client, |message| {
        matches!(message, Message::ContainerUpdate { container: c, items }
            if *c == container && items.is_empty())
    });

    // Повторное E закрывает панель.
    client.interact(3);
    expect_message(&mut session, &client, |message| {
        matches!(message, Message::ContainerUpdate { container: 0, .. })
    });
}

#[test]
fn drop_slot_replicates_ground_loot() {
    let (mut session, addr) = test_session(10);
    let client = TestClient::connect(addr);
    client.hello(Scene::demo_zone().content_hash());
    let player_id = expect_welcome(&mut session, &client);
    let catalog = test_catalog();
    let bolt = catalog.index_of("bolt").unwrap();

    assert_eq!(session.debug_give(player_id, "bolt", 3).unwrap(), 3);
    client.inventory_action(1, InventoryAction::Drop { slot: 0 });

    // В demo_zone артефакты лежат на земле с начала рейда — ищем именно болты.
    let update = expect_message(&mut session, &client, |message| {
        matches!(
            message,
            Message::Snapshot { loot, .. }
                if loot.iter().any(|item| item.item == bolt && item.count == 3)
        )
    });
    let Message::Snapshot { loot, .. } = update else {
        unreachable!("предикат гарантирует Snapshot")
    };
    let dropped: Vec<_> = loot.iter().filter(|item| item.item == bolt).collect();
    assert_eq!(dropped.len(), 1);
    assert_eq!(dropped[0].count, 3);

    // Инвентарь опустел: приходит персональный апдейт с пустыми ячейками.
    expect_message(&mut session, &client, |message| {
        matches!(message, Message::InventoryUpdate { slots }
            if slots.iter().flatten().next().is_none())
    });
}

#[test]
fn corpse_carries_victim_name_and_loot() {
    let (mut session, addr) = test_session(10);
    let shooter = TestClient::connect(addr);
    let victim = TestClient::connect(addr);
    let scene = Scene::demo_zone();
    shooter.hello(scene.content_hash());
    expect_welcome(&mut session, &shooter);
    victim.hello(scene.content_hash());
    let victim_id = expect_welcome(&mut session, &victim);

    // Лут жертвы переезжает в труп; патроны стрелку (M4d-3a).
    assert_eq!(session.debug_give(victim_id, "bolt", 2).unwrap(), 2);
    for client in session.clients() {
        assert_eq!(
            session
                .debug_give(client.player_id, "ammo_545", 40)
                .unwrap(),
            40
        );
    }

    // Упорный огонь стрелка: два хедшота убивают (капсулы в одной точке).
    let mut corpses = Vec::new();
    for tick in 0..40u64 {
        let mut input = ClientInput::new(tick);
        input.buttons = FLAG_FIRE;
        shooter.send(&Message::InputPacket {
            inputs: vec![input],
        });
        session.poll(Duration::from_millis(33));
        while let Some(message) = shooter.try_recv() {
            if let Message::Snapshot { corpses: seen, .. } = message {
                if !seen.is_empty() {
                    corpses = seen;
                }
            }
        }
        let _ = victim.try_recv();
        if !corpses.is_empty() {
            break;
        }
    }

    assert_eq!(corpses.len(), 1, "должен появиться труп жертвы");
    assert_eq!(corpses[0].owner, victim_id);
    assert_eq!(corpses[0].name, "Тестер", "имя жертвы в снапшоте");
    let items = session
        .simulation()
        .container_items(corpses[0].container)
        .expect("контейнер трупа существует");
    assert!(items.iter().any(|stack| stack.count == 2));
}

#[test]
fn medkit_heals_over_udp() {
    let (mut session, addr) = test_session(10);
    let client = TestClient::connect(addr);
    client.hello(Scene::demo_zone().content_hash());
    let player_id = expect_welcome(&mut session, &client);

    // Отладочный стрелок в той же точке: выстрел в упор — хедшот (50 урона).
    let shooter = session.debug_spawn();
    assert_eq!(session.debug_give(shooter, "ammo_545", 30).unwrap(), 30);
    let mut fire = ClientInput::new(0);
    fire.buttons = FLAG_FIRE;
    session.simulation().queue_input(shooter, fire);
    session.simulation().step_once();
    let mut stop = ClientInput::new(1);
    stop.buttons = 0;
    session.simulation().queue_input(shooter, stop);
    session.simulation().step_once();

    let wounded = session.simulation().player_state(player_id).unwrap();
    assert_eq!(wounded.health, 50, "хедшот в упор: 100 − 50");

    assert_eq!(session.debug_give(player_id, "medkit", 1).unwrap(), 1);
    client.inventory_action(5, InventoryAction::Use { slot: 0 });

    // Аптечка лечит 2 с (120 тиков); пинги продлевают сессию клиента.
    let mut healed = false;
    for round in 0..140u64 {
        if round % 20 == 0 {
            client.send(&Message::Ping {
                nonce: round as u32,
            });
        }
        session.poll(Duration::from_millis(33));
        let _ = client.try_recv();
        if session
            .simulation()
            .player_state(player_id)
            .is_some_and(|state| state.health == 100)
        {
            healed = true;
            break;
        }
    }
    assert!(healed, "аптечка должна вылечить до максимума");
    let state = session.simulation().player_state(player_id).unwrap();
    assert!(state.inventory[0].is_none(), "аптечка израсходована");
}

// ── Аномалии и болты (M4c) ──────────────────────────────────────────────

/// Индекс аномалии demo_zone в указанной точке (проверка квантов сцены).
fn anomaly_index_at(scene: &Scene, position: Vec3) -> u32 {
    let index = scene
        .anomalies
        .iter()
        .position(|anomaly| anomaly.position.distance(position) < 0.01)
        .expect("аномалия в точке есть в demo_zone");
    u32::try_from(index).unwrap()
}

/// Ведёт игрока к точке напрямую инпутами в симуляцию (без сети): поворот в
/// цель за один тик, затем шаг вперёд до дистанции `stop`. Завершается
/// нулевым инпутом: сервер повторяет последний, иначе игрок шагал бы вечно.
fn walk_to(sim: &mut Simulation, player_id: u64, target: Vec3, stop: f32) {
    let state = sim.player_state(player_id).unwrap();
    let to = target - state.position;
    let mut turn = ClientInput::new(sim.tick());
    turn.yaw_delta = to.z.atan2(to.x) - state.yaw;
    sim.queue_input(player_id, turn);
    sim.step_once();

    for _ in 0..1200 {
        let state = sim.player_state(player_id).unwrap();
        if state.position.distance(target) <= stop {
            let stand = ClientInput::new(sim.tick());
            sim.queue_input(player_id, stand);
            sim.step_once();
            return;
        }
        let mut command = ClientInput::new(sim.tick());
        command.move_forward = 1.0;
        sim.queue_input(player_id, command);
        sim.step_once();
    }
    panic!("игрок не дошёл до {target:?}");
}

/// Наводит прицел точно на точку: yaw — по горизонтали, pitch — явно (возврат
/// состояния вместо инпутов: повторяемый `yaw_delta` крутил бы игрока).
fn aim_at(sim: &mut Simulation, player_id: u64, target: Vec3, pitch: f32) {
    let state = sim.player_state(player_id).unwrap();
    let to = target - state.position;
    let mut net = state.net_state();
    net.yaw = to.z.atan2(to.x);
    net.pitch = pitch;
    assert!(sim.restore_player(player_id, &net), "игрок существует");
}

#[test]
fn throw_bolt_replicates_lands_and_dedupes() {
    let (mut session, addr) = test_session(10);
    let client = TestClient::connect(addr);
    client.hello(Scene::demo_zone().content_hash());
    let player_id = expect_welcome(&mut session, &client);
    let catalog = test_catalog();
    let bolt = catalog.index_of("bolt").unwrap();
    assert_eq!(session.debug_give(player_id, "bolt", 2).unwrap(), 2);

    // Прицел на -X (вдоль коридора мимо колонн), затем бросок через UDP.
    // Повторный пакет с тем же tick сервер отбрасывает.
    let spawn = Vec3::new(0.0, 0.0, 14.0);
    aim_at(session.simulation(), player_id, spawn - Vec3::X, 0.0);
    client.send(&Message::ThrowBolt { tick: 100 });
    client.send(&Message::ThrowBolt { tick: 100 });

    let mut saw_bolt = false;
    let mut max_flying = 0usize;
    for _ in 0..60 {
        session.poll(Duration::from_millis(33));
        while let Some(message) = client.try_recv() {
            if let Message::Snapshot { bolts, .. } = message {
                max_flying = max_flying.max(bolts.len());
                if bolts.iter().any(|bolt| bolt.owner == player_id) {
                    saw_bolt = true;
                }
            }
        }
        if saw_bolt {
            break;
        }
    }
    assert!(saw_bolt, "болт должен реплицироваться в снапшотах");
    assert_eq!(max_flying, 1, "дубликат ThrowBolt не создаёт второй болт");

    // Один болт потрачен из инвентаря, один остался.
    let state = session.simulation().player_state(player_id).unwrap();
    let left: u32 = state
        .inventory
        .iter()
        .flatten()
        .filter(|stack| stack.item == bolt)
        .map(|stack| stack.count)
        .sum();
    assert_eq!(left, 1);

    // Болт приземляется и лежит на земле как обычный предмет.
    let mut landed = false;
    for _ in 0..80 {
        session.poll(Duration::from_millis(33));
        while let Some(message) = client.try_recv() {
            if let Message::Snapshot { loot, bolts, .. } = message {
                if loot.iter().any(|item| item.item == bolt) {
                    landed = true;
                }
                assert!(bolts.len() <= 1, "лишние болты в полёте: {bolts:?}");
            }
        }
        if landed {
            break;
        }
    }
    assert!(landed, "болт должен приземлиться и стать предметом");
}

#[test]
fn bolt_in_anomaly_reveals_it_over_udp() {
    let (mut session, addr) = test_session(10);
    let client = TestClient::connect(addr);
    let scene = Scene::demo_zone();
    client.hello(scene.content_hash());
    let player_id = expect_welcome(&mut session, &client);
    let catalog = test_catalog();
    let bolt = catalog.index_of("bolt").unwrap();
    assert_eq!(session.debug_give(player_id, "bolt", 1).unwrap(), 1);

    // Подходим к жгучей аномалии (15, 15) на 5.5 м — снаружи сферы — и
    // бросаем болт: он влетает в сферу до того, как упадёт на пол.
    let heat = Vec3::new(15.0, 0.0, 15.0);
    let index = anomaly_index_at(&scene, heat);
    walk_to(session.simulation(), player_id, heat, 5.5);
    aim_at(session.simulation(), player_id, heat, 0.0);
    client.send(&Message::ThrowBolt { tick: 300 });

    let mut revealed = false;
    let mut saw_bolt = false;
    for _ in 0..60 {
        session.poll(Duration::from_millis(33));
        while let Some(message) = client.try_recv() {
            if let Message::Snapshot {
                bolts,
                events,
                loot,
                ..
            } = message
            {
                if !bolts.is_empty() {
                    saw_bolt = true;
                }
                if events.iter().any(|event| {
                    matches!(event, GameEvent::AnomalyRevealed { anomaly, .. } if *anomaly == index)
                }) {
                    revealed = true;
                }
                assert!(
                    !loot.iter().any(|item| item.item == bolt),
                    "болт в аномалии гибнет, а не падает на землю"
                );
            }
        }
        if revealed {
            break;
        }
    }
    assert!(saw_bolt, "летящий болт виден в снапшотах");
    assert!(revealed, "AnomalyRevealed должен дойти до клиента");
}

#[test]
fn heat_anomaly_kills_player_with_anomaly_killer() {
    let (mut session, addr) = test_session(10);
    let client = TestClient::connect(addr);
    client.hello(Scene::demo_zone().content_hash());
    let player_id = expect_welcome(&mut session, &client);
    let catalog = test_catalog();
    let medkit = catalog.index_of("medkit").unwrap();
    assert_eq!(session.debug_give(player_id, "medkit", 1).unwrap(), 1);

    // Идём в центр жгучей аномалии (15, 15): урон копится, пока стоит.
    walk_to(
        session.simulation(),
        player_id,
        Vec3::new(15.0, 0.0, 15.0),
        0.5,
    );
    assert!(
        session.simulation().player_state(player_id).unwrap().health < 100,
        "стоя в аномалии, игрок получает урон"
    );

    let mut corpses = Vec::new();
    let mut death_named = false;
    for round in 0..600u64 {
        if round % 20 == 0 {
            client.send(&Message::Ping {
                nonce: round as u32,
            });
        }
        session.poll(Duration::from_millis(33));
        while let Some(message) = client.try_recv() {
            if let Message::Snapshot {
                corpses: seen,
                events,
                ..
            } = message
            {
                if !seen.is_empty() {
                    corpses = seen;
                }
                if events.iter().any(|event| {
                    matches!(
                        event,
                        GameEvent::Death { victim, killer: 0, killer_name, .. }
                            if *victim == player_id && killer_name == "Аномалия"
                    )
                }) {
                    death_named = true;
                }
            }
        }
        if death_named {
            break;
        }
    }

    assert!(death_named, "в килл-фиде смерть подписана «Аномалия»");
    assert_eq!(corpses.len(), 1, "должен появиться обычный труп");
    assert_eq!(corpses[0].owner, player_id);
    assert_eq!(corpses[0].name, "Тестер");
    let items = session
        .simulation()
        .container_items(corpses[0].container)
        .expect("контейнер трупа существует");
    assert!(
        items
            .iter()
            .any(|stack| stack.item == medkit && stack.count == 1),
        "лут жертвы в трупе: {items:?}"
    );
}

#[test]
fn artifact_picked_up_in_anomaly() {
    let (mut session, addr) = test_session(10);
    let client = TestClient::connect(addr);
    let scene = Scene::demo_zone();
    client.hello(scene.content_hash());
    let player_id = expect_welcome(&mut session, &client);
    let catalog = test_catalog();
    let artifact = catalog.index_of("artifact_heat").unwrap();

    // Артефакт «Жар-камень» лежит в якоре жгучей аномалии (15, 15) с начала рейда.
    let heat = Vec3::new(15.0, 0.0, 15.0);
    let index = anomaly_index_at(&scene, heat);
    assert_eq!(
        scene.anomalies[usize::try_from(index).unwrap()]
            .artifact
            .as_deref(),
        Some("artifact_heat")
    );

    // Подходим почти вплотную и смотрим вниз (предмет на полу под ногами).
    walk_to(session.simulation(), player_id, heat, 0.8);
    aim_at(session.simulation(), player_id, heat, -1.05);

    client.interact(500);
    expect_message(&mut session, &client, |message| {
        matches!(message, Message::InventoryUpdate { slots }
            if slots.iter().flatten().any(|stack| stack.item == artifact))
    });
}

// ── Мир локаций (M4d-1) ─────────────────────────────────────────────────

#[test]
fn world_handshake_reports_base_and_rejects_scene_hash() {
    let (mut session, addr) = test_world_session(10);
    let client = TestClient::connect(addr);
    client.hello(test_world_hash());
    let reply = expect_message(&mut session, &client, |message| {
        matches!(
            message,
            Message::ServerWelcome { .. } | Message::ServerReject { .. }
        )
    });

    match reply {
        Message::ServerWelcome {
            player_id,
            spawn,
            location,
            ..
        } => {
            assert_eq!(player_id, 1);
            assert_eq!(location, LOCATION_BASE);
            assert_eq!(spawn, Scene::base().spawn.to_array());
        }
        other => panic!("ожидали Welcome, получили {other:?}"),
    }

    // Хэш одной сцены вместо мирового — отказ в подключении.
    let second = TestClient::connect(addr);
    second.hello(Scene::kordon().content_hash());
    let reply = expect_message(&mut session, &second, |message| {
        matches!(message, Message::ServerReject { .. })
    });
    assert_eq!(
        reply,
        Message::ServerReject {
            reason: RejectReason::ContentMismatch
        }
    );
    assert_eq!(session.player_count(), 1);
}

#[test]
fn snapshots_cover_only_the_players_location() {
    let (mut session, addr) = test_world_session(10);
    let client = TestClient::connect(addr);
    client.hello(test_world_hash());
    let player_id = expect_welcome(&mut session, &client);

    // Разведчик — в Свалке (в центре карты: ни пузырей, ни переходов).
    let scout = session.debug_spawn();
    assert!(session.debug_teleport(scout, LOCATION_SVALKA, Vec3::ZERO));
    assert_eq!(session.player_location(scout), Some(LOCATION_SVALKA));

    // Снапшот клиента на базе содержит только игроков базы.
    match expect_message(&mut session, &client, |message| {
        matches!(message, Message::Snapshot { .. })
    }) {
        Message::Snapshot { players, .. } => {
            assert_eq!(players.len(), 1, "чужие локации в снапшот не попадают");
            assert_eq!(players[0].player_id, player_id);
        }
        other => unreachable!("предикат гарантирует Snapshot: {other:?}"),
    }
    assert_eq!(
        session.simulation().player_count(),
        1,
        "база: только клиент"
    );
    assert_eq!(
        session
            .simulation_at(LOCATION_SVALKA)
            .unwrap()
            .player_count(),
        1,
        "Свалка: только разведчик"
    );
}

#[test]
fn snapshots_carry_active_bubbles_of_the_players_location() {
    let (mut session, addr) = test_world_session(10);
    let client = TestClient::connect(addr);
    client.hello(test_world_hash());
    let player_id = expect_welcome(&mut session, &client);

    // База: пузырей нет — снапшот пуст.
    match expect_message(&mut session, &client, |message| {
        matches!(message, Message::Snapshot { .. })
    }) {
        Message::Snapshot { bubbles, .. } => assert!(bubbles.is_empty(), "{bubbles:?}"),
        other => unreachable!("предикат гарантирует Snapshot: {other:?}"),
    }

    // Игрок уходит на Свалку (в центр: ни пузырей, ни переходов) — снапшот
    // приходит уже с активными якорями Свалки.
    assert!(session.debug_teleport(player_id, LOCATION_SVALKA, Vec3::ZERO));
    let expected: Vec<[i16; 3]> = session
        .active_bubbles(LOCATION_SVALKA)
        .into_iter()
        .map(|anchor| quantize_pos(anchor.to_array()))
        .collect();
    assert!(!expected.is_empty(), "на Свалке есть активные пузыри");
    let reply = expect_message(
        &mut session,
        &client,
        |message| matches!(message, Message::Snapshot { bubbles, .. } if !bubbles.is_empty()),
    );
    let Message::Snapshot { bubbles, .. } = reply else {
        unreachable!("предикат гарантирует Snapshot: {reply:?}")
    };
    assert_eq!(bubbles, expected, "снапшот несёт якоря активных пузырей");
}

#[test]
fn transition_walks_kordon_and_back_with_deposit() {
    let (mut session, _addr) = test_world_session(10);
    let player_id = session.debug_spawn();
    let catalog = test_catalog();
    let bolt = catalog.index_of("bolt").unwrap();
    assert_eq!(session.debug_give(player_id, "bolt", 3).unwrap(), 3);

    // База: идём в ворота (0,0,-20) — переход на Кордон.
    walk_to(
        session.simulation(),
        player_id,
        Vec3::new(0.0, 0.0, -20.0),
        0.5,
    );
    assert!(
        poll_until(&mut session, 20, |session| {
            session.player_location(player_id) == Some(LOCATION_KORDON)
        }),
        "переход на Кордон не сработал"
    );
    let state = session
        .simulation_at(LOCATION_KORDON)
        .unwrap()
        .player_state(player_id)
        .unwrap();
    assert!(
        state.position.distance(Vec3::new(0.0, 0.0, 22.0)) < 0.5,
        "точка прибытия у ворот Кордона: {:?}",
        state.position
    );
    assert!(
        state
            .inventory
            .iter()
            .flatten()
            .any(|stack| stack.item == bolt && stack.count == 3),
        "лут едет с игроком"
    );

    // Кордон: идём в зону «База» (0,0,26) — возвращаемся с лутом.
    walk_to(
        session.simulation_at(LOCATION_KORDON).unwrap(),
        player_id,
        Vec3::new(0.0, 0.0, 26.0),
        0.5,
    );
    assert!(
        poll_until(&mut session, 40, |session| {
            session.player_location(player_id) == Some(LOCATION_BASE)
        }),
        "возврат на базу не сработал"
    );
    let state = session.simulation().player_state(player_id).unwrap();
    assert!(
        state.position.distance(Vec3::new(0.0, 0.0, -16.0)) < 0.5,
        "точка прибытия у ворот базы: {:?}",
        state.position
    );
    assert!(
        state.inventory.iter().all(Option::is_none),
        "вход на базу сдаёт весь инвентарь"
    );
    assert_eq!(
        session.stash_items(player_id),
        &[Stack::new(bolt, 3)][..],
        "лут переехал в склад"
    );

    // Точка прибытия вне зон перехода — петли нет.
    poll_until(&mut session, 10, |_| false);
    assert_eq!(session.player_location(player_id), Some(LOCATION_BASE));
}

#[test]
fn bubble_teleports_to_kordon_arrivals_and_respawns() {
    let (mut session, _addr) = test_world_session(10);
    let player_id = session.debug_spawn();

    // Активный пузырь Свалки: переносим игрока точно на его точку.
    let before = session.active_bubbles(LOCATION_SVALKA);
    assert!(!before.is_empty(), "на Свалке есть активные пузыри");
    assert!(session.debug_teleport(player_id, LOCATION_SVALKA, before[0]));
    assert!(
        poll_until(&mut session, 20, |session| {
            session.player_location(player_id) == Some(LOCATION_KORDON)
        }),
        "пузырь не унёс игрока на Кордон"
    );
    assert_eq!(
        session.active_bubbles(LOCATION_SVALKA).len(),
        before.len() - 1,
        "сработавший пузырь гаснет"
    );

    // Высадка — в одной из точек прибытия Кордона («почти под базу»).
    let state = session
        .simulation_at(LOCATION_KORDON)
        .unwrap()
        .player_state(player_id)
        .unwrap();
    let arrivals = Scene::kordon().bubble_arrivals;
    assert!(
        arrivals
            .iter()
            .any(|arrival| arrival.distance(state.position) < 0.5),
        "прибытие пузыря у ворот базы: {:?}",
        state.position
    );

    // Через ~90 с использованный пузырь возвращается в свободную точку:
    // шагаем по тику (память FixedStep ограничивает catch-up 8 шагами).
    let consumed_tick = session.tick();
    let mut respawned = false;
    for _ in 0..6000 {
        session.poll(Duration::from_millis(17));
        if session.active_bubbles(LOCATION_SVALKA).len() == before.len() {
            respawned = true;
            break;
        }
    }
    assert!(respawned, "пузырь должен вернуться в мир");
    assert!(
        session.tick() >= consumed_tick + 90 * 60,
        "возврат не раньше 90 с: {} тиков",
        session.tick() - consumed_tick
    );
}

#[test]
fn stash_put_take_over_udp_and_dedupe() {
    let (mut session, addr) = test_world_session(10);
    let client = TestClient::connect(addr);
    client.hello(test_world_hash());
    let player_id = expect_welcome(&mut session, &client);
    let catalog = test_catalog();
    let bolt = catalog.index_of("bolt").unwrap();
    let medkit = catalog.index_of("medkit").unwrap();

    // Кладём ячейку: склад получает стопку целиком.
    assert_eq!(session.debug_give(player_id, "bolt", 3).unwrap(), 3);
    client.stash_action(10, StashAction::Put { slot: 0 });
    let reply = expect_message(&mut session, &client, |message| {
        matches!(message, Message::StashUpdate { .. })
    });
    let Message::StashUpdate { items } = reply else {
        unreachable!("предикат гарантирует StashUpdate: {reply:?}")
    };
    assert_eq!(items.len(), 1);
    assert_eq!((items[0].item, items[0].count), (bolt, 3));

    // Повтор с тем же tick отброшен (дедупликация действий).
    assert_eq!(session.debug_give(player_id, "medkit", 1).unwrap(), 1);
    client.stash_action(10, StashAction::Put { slot: 0 });
    for _ in 0..20 {
        session.poll(Duration::from_millis(33));
    }
    let mut saw_medkit = false;
    while let Some(message) = client.try_recv() {
        if let Message::StashUpdate { items } = message {
            saw_medkit = items.iter().any(|stack| stack.item == medkit);
        }
    }
    assert!(!saw_medkit, "повтор с тем же tick не кладёт предмет");

    // Новый tick — медикамент в складе рядом с болтами (стопки не сливаются).
    client.stash_action(11, StashAction::Put { slot: 0 });
    expect_message(
        &mut session,
        &client,
        |message| matches!(message, Message::StashUpdate { items } if items.len() == 2),
    );
    assert_eq!(
        session
            .stash_items(player_id)
            .iter()
            .map(|stack| stack.item)
            .collect::<Vec<_>>(),
        [bolt, medkit]
    );

    // Забираем болты обратно: склад худеет, инвентарь полнеет.
    client.stash_action(12, StashAction::Take { index: 0 });
    expect_message(&mut session, &client, |message| {
        matches!(message, Message::StashUpdate { items }
            if items.iter().all(|stack| stack.item == medkit))
    });
    expect_message(&mut session, &client, |message| {
        matches!(message, Message::InventoryUpdate { slots }
            if slots.iter().flatten().any(|stack| stack.item == bolt && stack.count == 3))
    });
}

#[test]
fn stash_actions_require_base_location() {
    let (mut session, addr) = test_world_session(10);
    let client = TestClient::connect(addr);
    client.hello(test_world_hash());
    let player_id = expect_welcome(&mut session, &client);
    assert_eq!(session.debug_give(player_id, "bolt", 2).unwrap(), 2);

    // На Кордоне склад недоступен: действие молча отбрасывается.
    assert!(session.debug_teleport(player_id, LOCATION_KORDON, Vec3::new(0.0, 0.0, 22.0)));
    client.stash_action(20, StashAction::Put { slot: 0 });
    for _ in 0..30 {
        session.poll(Duration::from_millis(33));
    }
    let mut saw_stash = false;
    while let Some(message) = client.try_recv() {
        if matches!(message, Message::StashUpdate { .. }) {
            saw_stash = true;
        }
    }
    assert!(!saw_stash, "вне базы склад молчит");
    assert!(session.stash_items(player_id).is_empty());
}

#[test]
fn extraction_saves_percent_and_returns_player_to_base() {
    let (mut session, addr) = test_world_session(10);
    // Второй клиент остаётся на Кордоне — ему приходит событие эвакуации.
    let watcher = TestClient::connect(addr);
    watcher.hello(test_world_hash());
    let watcher_id = expect_welcome(&mut session, &watcher);

    let player_id = session.debug_spawn();
    let catalog = test_catalog();
    let medkit = catalog.index_of("medkit").unwrap();
    let canned = catalog.index_of("canned_food").unwrap();
    let bolt = catalog.index_of("bolt").unwrap();
    assert_eq!(session.debug_give(player_id, "medkit", 2).unwrap(), 2);
    assert_eq!(session.debug_give(player_id, "canned_food", 2).unwrap(), 2);
    assert_eq!(session.debug_give(player_id, "bolt", 4).unwrap(), 4);

    // Ведём наблюдателя на Кордон (через ворота базы).
    walk_to(
        session.simulation(),
        watcher_id,
        Vec3::new(0.0, 0.0, -20.0),
        0.5,
    );
    assert!(
        poll_until(&mut session, 40, |session| {
            session.player_location(watcher_id) == Some(LOCATION_KORDON)
        }),
        "наблюдатель не дошёл до Кордона"
    );

    // Игрок: база → Кордон → круг аварийной эвакуации в дальнем углу.
    walk_to(
        session.simulation(),
        player_id,
        Vec3::new(0.0, 0.0, -20.0),
        0.5,
    );
    assert!(
        poll_until(&mut session, 40, |session| {
            session.player_location(player_id) == Some(LOCATION_KORDON)
        }),
        "игрок не перешёл на Кордон"
    );
    let circle = session.world_scene(LOCATION_KORDON).extractions[0].position;
    walk_to(
        session.simulation_at(LOCATION_KORDON).unwrap(),
        player_id,
        circle,
        0.5,
    );

    // Событие эвакуации приходит наблюдателю в снапшоте Кордона.
    let extracted = expect_message(&mut session, &watcher, |message| {
        matches!(
            message,
            Message::Snapshot { events, .. }
                if events.iter().any(|event| matches!(
                    event,
                    GameEvent::Extracted { player, percent, .. }
                        if *player == player_id && (*percent - 0.75).abs() < 1e-6
                ))
        )
    });
    assert!(
        matches!(extracted, Message::Snapshot { .. }),
        "ожидался снапшот с событием Extracted"
    );

    // Игрок — на базе с пустым инвантарём и урезанным складом.
    assert!(
        poll_until(&mut session, 20, |session| {
            session.player_location(player_id) == Some(LOCATION_BASE)
        }),
        "экстракция не вернула игрока на базу"
    );
    let state = session.simulation().player_state(player_id).unwrap();
    assert!(
        state.inventory.iter().all(Option::is_none),
        "инвентарь после экстракции пуст"
    );
    assert_eq!(
        session.stash_items(player_id),
        &[
            Stack::new(medkit, 2),
            Stack::new(canned, 1),
            Stack::new(bolt, 3),
        ][..],
        "75 % добычи по весу ушли в склад"
    );
    // Остаток лежит в круге на Кордоне — его может подобрать кто угодно.
    let loot = session
        .simulation_at(LOCATION_KORDON)
        .unwrap()
        .snapshot_loot();
    assert!(
        loot.iter()
            .any(|item| item.item == canned && item.count == 1)
            && loot.iter().any(|item| item.item == bolt && item.count == 1),
        "потерянная часть добычи на земле в круге: {loot:?}"
    );
}

#[test]
fn profile_restores_stash_after_reconnect() {
    let (mut session, addr) = test_world_session(10);
    let client = TestClient::connect(addr);
    client.hello_profile(test_world_hash(), Some("stalker-1"));
    let player_id = expect_welcome(&mut session, &client);
    assert_eq!(session.debug_give(player_id, "bolt", 5).unwrap(), 5);

    // Экстракция кругом Кордона: склад профиля наполняется.
    walk_to(
        session.simulation(),
        player_id,
        Vec3::new(0.0, 0.0, -20.0),
        0.5,
    );
    assert!(
        poll_until(&mut session, 40, |session| {
            session.player_location(player_id) == Some(LOCATION_KORDON)
        }),
        "игрок не перешёл на Кордон"
    );
    let circle = session.world_scene(LOCATION_KORDON).extractions[0].position;
    walk_to(
        session.simulation_at(LOCATION_KORDON).unwrap(),
        player_id,
        circle,
        0.5,
    );
    assert!(
        poll_until(&mut session, 20, |session| {
            session.player_location(player_id) == Some(LOCATION_BASE)
        }),
        "экстракция не сработала"
    );
    let stashed = session.stash_items(player_id).to_vec();
    // bolt×5 = 0.25 кг; бюджет 75 % = 0.1875 кг → 3 болта в склад, 2 падают.
    assert_eq!(stashed, vec![Stack::new(catalog_index("bolt"), 3)]);

    // Отключение и реконнект с тем же профилем: склад на месте.
    client.send(&Message::ClientBye);
    poll_until(&mut session, 10, |session| session.clients().is_empty());
    let again = TestClient::connect(addr);
    again.hello_profile(test_world_hash(), Some("stalker-1"));
    let new_id = expect_welcome(&mut session, &again);
    assert_ne!(new_id, player_id, "реконнект — новый player_id");
    assert_eq!(
        session.stash_items(new_id),
        &stashed[..],
        "склад профиля восстановлен"
    );

    // Без профиля склад не переживает сессию.
    let other = TestClient::connect(addr);
    other.hello(test_world_hash());
    let other_id = expect_welcome(&mut session, &other);
    assert!(
        session.stash_items(other_id).is_empty(),
        "беспрофильный игрок начинает с пустым складом"
    );
    other.send(&Message::ClientBye);
    poll_until(&mut session, 10, |session| session.clients().len() == 1);
    let third = TestClient::connect(addr);
    third.hello(test_world_hash());
    let third_id = expect_welcome(&mut session, &third);
    assert_ne!(third_id, other_id);
    assert!(
        session.stash_items(third_id).is_empty(),
        "склад беспрофильного игрока умер с сессией"
    );
}

/// Индекс предмета в каталоге контента (тесты e2e читают тот же каталог).
fn catalog_index(key: &str) -> u16 {
    test_catalog().index_of(key).expect("предмет в каталоге")
}
