//! E2E-тесты UDP-сессии на loopback: сервер и «тупые» клиенты через `zn-net`.
//! Время сессии виртуальное (`Session::poll(dt)`), wall-clock-ожиданий нет —
//! только короткие `yield_now` для доставки loopback-датаграмм.

use std::net::{SocketAddr, UdpSocket};
use std::sync::OnceLock;
use std::time::Duration;

use zn_net::{
    decode_message, encode_message, ClientInput, GameEvent, InventoryAction, Message,
    PlayerNetState, RejectReason, FLAG_FIRE, FLAG_GROUNDED, PROTOCOL_VERSION,
};
use zn_scene::Scene;
use zn_server::{Session, SessionConfig};
use zn_sim::crate_container_id;

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
        self.send(&Message::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            content_hash,
            items_hash: test_items_hash(),
            name: "Тестер".to_string(),
        });
    }

    /// Обыск/подбор (E) без redundancy — тесты сами контролируют повторы.
    fn interact(&self, tick: u64) {
        self.send(&Message::Interact { tick });
    }

    fn inventory_action(&self, tick: u64, action: InventoryAction) {
        self.send(&Message::InventoryAction { tick, action });
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

    let update = expect_message(
        &mut session,
        &client,
        |message| matches!(message, Message::Snapshot { loot, .. } if !loot.is_empty()),
    );
    let Message::Snapshot { loot, .. } = update else {
        unreachable!("предикат гарантирует Snapshot")
    };
    assert_eq!(loot.len(), 1);
    assert_eq!(loot[0].item, bolt);
    assert_eq!(loot[0].count, 3);

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

    // Лут жертвы переезжает в труп.
    assert_eq!(session.debug_give(victim_id, "bolt", 2).unwrap(), 2);

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
