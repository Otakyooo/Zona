use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use zn_content::ItemCatalog;
use zn_net::{
    check_client_hello, decode_message, encode_message, ClientInput, CorpseNet, GameEvent,
    InventoryAction, Message, RejectReason, INPUT_REDUNDANCY, MAX_NAME_LEN, SERVER_TIMEOUT,
    SIMULATION_HZ, SNAPSHOT_HZ,
};
use zn_scene::Scene;
use zn_sim::{NetUpdates, PlayerAction, Simulation};

/// Порт по умолчанию для выделенного сервера.
pub const DEFAULT_PORT: u16 = 7700;

/// Буфер приёма: наш протокол без фрагментации укладывается в MTU.
const RECV_BUF_SIZE: usize = 2048;

/// Снапшот каждые `SIMULATION_HZ / SNAPSHOT_HZ` тиков симуляции (2 при 60/30).
const SNAPSHOT_EVERY_TICKS: u64 = (SIMULATION_HZ / SNAPSHOT_HZ) as u64;

/// Параметры сессии. Всё, что влияет на детерминизм тестов (латентность/потери
/// исходящих), по умолчанию выключено.
#[derive(Clone, Debug)]
pub struct SessionConfig {
    pub max_players: usize,
    /// Задержка исходящих пакетов — диагностика джиттера, не для CI.
    pub out_latency: Duration,
    /// Процент дропа исходящих пакетов (0..=100) — диагностика потерь.
    pub out_loss_pct: f32,
    /// Seed детерминированного генератора потерь (воспроизводимость).
    pub rng_seed: u32,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            max_players: zn_net::MAX_PLAYERS_PER_SESSION,
            out_latency: Duration::ZERO,
            out_loss_pct: 0.0,
            rng_seed: 0x9E37_79B9,
        }
    }
}

/// Взгляд на подключённого клиента (для консоли и тестов).
#[derive(Clone, Debug)]
pub struct ClientInfo {
    pub player_id: u64,
    pub name: String,
    pub addr: SocketAddr,
    /// Виртуальное время с момента последнего пакета клиента.
    pub idle: Duration,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SessionStats {
    pub received: u64,
    pub sent: u64,
    pub dropped: u64,
}

struct ClientState {
    player_id: u64,
    name: String,
    addr: SocketAddr,
    last_seen: Duration,
    /// Последний обработанный `Interact` (дедупликация повторов клиента).
    last_interact_tick: u64,
    /// Последний обработанный `InventoryAction`.
    last_action_tick: u64,
}

/// Один процесс сервера = один матч. Сессия владеет сокетом и симуляцией,
/// весь тайминг идёт от `poll(dt)` — время виртуальное, поэтому e2e-тесты
/// гоняют реальные сокеты без wall-clock-ожиданий.
pub struct Session {
    socket: UdpSocket,
    simulation: Simulation,
    content_hash: u64,
    items_hash: u64,
    /// История имён по id: килл-фид и подписи трупов переживают отключение.
    names: HashMap<u64, String>,
    config: SessionConfig,
    clients: HashMap<SocketAddr, ClientState>,
    now: Duration,
    last_snapshot_tick: u64,
    outbox: VecDeque<(Duration, SocketAddr, Vec<u8>)>,
    rng: u32,
    stats: SessionStats,
}

impl Session {
    /// Биндит UDP-сокет по адресу (`127.0.0.1:0` в тестах) и создаёт сессию.
    pub fn bind(
        bind_addr: &str,
        scene: &Scene,
        catalog: ItemCatalog,
        config: SessionConfig,
    ) -> io::Result<Self> {
        let socket = UdpSocket::bind(bind_addr)?;
        Self::new(socket, scene, catalog, config)
    }

    /// Сессия на готовом сокете (переводится в non-blocking). Каталог предметов
    /// наполняет ящики сцены; его хэш сверяется в handshake.
    pub fn new(
        socket: UdpSocket,
        scene: &Scene,
        catalog: ItemCatalog,
        config: SessionConfig,
    ) -> io::Result<Self> {
        socket.set_nonblocking(true)?;
        let items_hash = catalog.content_hash();
        let simulation = Simulation::with_scene_and_catalog(scene, catalog);
        let spawn = simulation.spawn_point();
        log::info!(
            "сессия: уровень '{}', {} статических коллайдеров, спавн {spawn:?}, hash={:#018x}, items={:#018x}",
            scene.name,
            simulation.static_collider_count(),
            scene.content_hash(),
            items_hash
        );
        let rng = config.rng_seed | 1;
        Ok(Self {
            socket,
            simulation,
            content_hash: scene.content_hash(),
            items_hash,
            names: HashMap::new(),
            config,
            clients: HashMap::new(),
            now: Duration::ZERO,
            last_snapshot_tick: 0,
            outbox: VecDeque::new(),
            rng,
            stats: SessionStats::default(),
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub fn content_hash(&self) -> u64 {
        self.content_hash
    }

    /// Хэш каталога предметов (клиенты сверяют его в Hello).
    pub fn items_hash(&self) -> u64 {
        self.items_hash
    }

    pub fn max_players(&self) -> usize {
        self.config.max_players
    }

    pub fn tick(&self) -> u64 {
        self.simulation.tick()
    }

    /// Виртуальное время сессии.
    pub fn now(&self) -> Duration {
        self.now
    }

    pub fn stats(&self) -> SessionStats {
        self.stats
    }

    pub fn player_count(&mut self) -> usize {
        self.simulation.player_count()
    }

    pub fn clients(&self) -> Vec<ClientInfo> {
        self.clients
            .values()
            .map(|client| ClientInfo {
                player_id: client.player_id,
                name: client.name.clone(),
                addr: client.addr,
                idle: self.now.saturating_sub(client.last_seen),
            })
            .collect()
    }

    /// Симуляция для диагностики/тестов (состояния игроков и т.п.).
    pub fn simulation(&mut self) -> &mut Simulation {
        &mut self.simulation
    }

    /// Кикает игрока по id: без пакетов, клиент сам истечёт по таймауту.
    pub fn kick(&mut self, player_id: u64) -> bool {
        let Some(addr) = self
            .clients
            .iter()
            .find(|(_, client)| client.player_id == player_id)
            .map(|(addr, _)| *addr)
        else {
            return false;
        };
        let client = self.clients.remove(&addr).expect("найден выше");
        self.simulation.despawn_player(client.player_id);
        log::info!(
            "игрок '{}' (id {}) кикнут ({})",
            client.name,
            client.player_id,
            client.addr
        );
        true
    }

    /// Отладочный игрок без сетевого клиента (консольная команда `spawn`).
    pub fn debug_spawn(&mut self) -> u64 {
        let spawn = self.simulation.spawn_point();
        let id = self.simulation.spawn_player(spawn);
        log::info!("отладочный игрок {id} заспавнен в {spawn:?}");
        id
    }

    /// Отладка: выдать предмет игроку по ключу каталога (консоль `loot`).
    pub fn debug_give(
        &mut self,
        player_id: u64,
        item_key: &str,
        count: u32,
    ) -> Result<u32, String> {
        let Some(item) = self.simulation.catalog_index_of(item_key) else {
            return Err(format!("предмет '{item_key}' не найден в каталоге"));
        };
        if self.simulation.player_state(player_id).is_none() {
            return Err(format!("игрок {player_id} не найден"));
        }
        Ok(self.simulation.debug_give(player_id, item, count))
    }

    /// Один шаг цикла: приём → фиксированные шаги симуляции → снапшоты
    /// (30 Гц) → таймауты → отправка отложенных пакетов.
    pub fn poll(&mut self, dt: Duration) {
        self.now += dt;
        self.receive_all();
        self.simulation.advance(dt);
        if self.simulation.tick() - self.last_snapshot_tick >= SNAPSHOT_EVERY_TICKS {
            self.last_snapshot_tick = self.simulation.tick();
            self.send_snapshots();
        }
        self.check_timeouts();
        self.flush_outbox();
    }

    fn receive_all(&mut self) {
        let mut buf = [0u8; RECV_BUF_SIZE];
        loop {
            match self.socket.recv_from(&mut buf) {
                Ok((len, addr)) => {
                    self.stats.received += 1;
                    match decode_message::<Message>(&buf[..len]) {
                        Ok(message) => self.handle_message(addr, message),
                        Err(error) => {
                            log::debug!("пакет от {addr} не декодирован: {error}");
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    log::debug!("recv: {error}");
                    break;
                }
            }
        }
    }

    fn handle_message(&mut self, addr: SocketAddr, message: Message) {
        match message {
            Message::ClientHello {
                protocol_version,
                content_hash,
                items_hash,
                name,
            } => self.handle_hello(addr, protocol_version, content_hash, items_hash, name),
            Message::InputPacket { inputs } => self.handle_inputs(addr, inputs),
            Message::Interact { tick } => self.handle_interact(addr, tick),
            Message::InventoryAction { tick, action } => {
                self.handle_inventory_action(addr, tick, action);
            }
            Message::ThrowBolt { tick } => self.handle_throw_bolt(addr, tick),
            Message::Ping { nonce } => {
                if let Some(client) = self.clients.get_mut(&addr) {
                    client.last_seen = self.now;
                }
                let server_tick = self.simulation.tick();
                self.send(addr, Message::Pong { nonce, server_tick });
            }
            Message::ClientBye => self.remove_client(addr, "вышел"),
            other => {
                log::debug!("игнорируем {} от {addr}", other.kind());
            }
        }
    }

    fn handle_hello(
        &mut self,
        addr: SocketAddr,
        protocol_version: u32,
        content_hash: u64,
        items_hash: u64,
        name: String,
    ) {
        // Повтор Hello (Welcome мог потеряться) — повторяем Welcome.
        if let Some(client) = self.clients.get_mut(&addr) {
            client.last_seen = self.now;
            let player_id = client.player_id;
            self.send_welcome(addr, player_id);
            return;
        }

        if self.clients.len() >= self.config.max_players {
            log::info!("отказ {addr}: сессия заполнена");
            self.send(
                addr,
                Message::ServerReject {
                    reason: RejectReason::SessionFull,
                },
            );
            return;
        }

        if let Err(reason) = check_client_hello(
            protocol_version,
            content_hash,
            items_hash,
            self.content_hash,
            self.items_hash,
        ) {
            log::info!("отказ {addr}: {reason:?}");
            self.send(addr, Message::ServerReject { reason });
            return;
        }

        let name = clamp_name(&name);
        let spawn = self.simulation.spawn_point();
        let player_id = self.simulation.spawn_player(spawn);
        log::info!(
            "игрок '{name}' (id {player_id}) подключился с {addr} [{} игроков]",
            self.clients.len() + 1
        );
        self.names.insert(player_id, name.clone());
        self.clients.insert(
            addr,
            ClientState {
                player_id,
                name,
                addr,
                last_seen: self.now,
                last_interact_tick: 0,
                last_action_tick: 0,
            },
        );
        self.send_welcome(addr, player_id);
    }

    fn send_welcome(&mut self, addr: SocketAddr, player_id: u64) {
        let server_tick = self.simulation.tick();
        let spawn = self.simulation.spawn_point().to_array();
        self.send(
            addr,
            Message::ServerWelcome {
                player_id,
                server_tick,
                spawn,
            },
        );
    }

    fn handle_inputs(&mut self, addr: SocketAddr, inputs: Vec<ClientInput>) {
        let Some(client) = self.clients.get_mut(&addr) else {
            log::debug!("InputPacket от неизвестного адреса {addr} — игнор");
            return;
        };
        client.last_seen = self.now;
        let player_id = client.player_id;

        // Берём только последние INPUT_REDUNDANCY инпутов (redundancy против
        // потерь) и санитизируем: ничего нефинитного/запредельного в sim.
        let skip = inputs.len().saturating_sub(INPUT_REDUNDANCY);
        for mut input in inputs.into_iter().skip(skip) {
            sanitize_input(&mut input);
            self.simulation.queue_input(player_id, input);
        }
    }

    /// Обыск/подбор (E): повторные пакеты с тем же tick отбрасываются —
    /// клиент шлёт действие с redundancy, но обработать его нужно один раз.
    fn handle_interact(&mut self, addr: SocketAddr, tick: u64) {
        let Some(client) = self.clients.get_mut(&addr) else {
            return;
        };
        client.last_seen = self.now;
        if tick <= client.last_interact_tick {
            return;
        }
        client.last_interact_tick = tick;
        let player_id = client.player_id;
        self.simulation
            .queue_action(player_id, PlayerAction::Interact);
    }

    /// Действие с инвентарём: та же дедупликация по tick, что у `Interact`.
    fn handle_inventory_action(&mut self, addr: SocketAddr, tick: u64, action: InventoryAction) {
        let Some(client) = self.clients.get_mut(&addr) else {
            return;
        };
        client.last_seen = self.now;
        if tick <= client.last_action_tick {
            return;
        }
        client.last_action_tick = tick;
        let player_id = client.player_id;
        self.simulation
            .queue_action(player_id, PlayerAction::Inventory(action));
    }

    /// Бросок болта (G): дедупликация по `tick`, общий счётчик с действиями.
    fn handle_throw_bolt(&mut self, addr: SocketAddr, tick: u64) {
        let Some(client) = self.clients.get_mut(&addr) else {
            return;
        };
        client.last_seen = self.now;
        if tick <= client.last_action_tick {
            return;
        }
        client.last_action_tick = tick;
        let player_id = client.player_id;
        self.simulation
            .queue_action(player_id, PlayerAction::ThrowBolt);
    }

    fn send_snapshots(&mut self) {
        let players = self.simulation.snapshot();
        let server_tick = self.simulation.tick();
        let loot = self.simulation.snapshot_loot();
        let mut corpses = self.simulation.snapshot_corpses();
        self.fill_corpse_names(&mut corpses);
        let bolts = self.simulation.snapshot_bolts();
        let events = self.simulation.take_events();
        let events = self.enrich_events(events);
        let updates = self.simulation.take_net_updates();

        let addrs: Vec<SocketAddr> = self.clients.keys().copied().collect();
        for addr in addrs {
            let Some(client) = self.clients.get(&addr) else {
                continue;
            };
            // ack персональный: сколько инпутов этого игрока уже учтено.
            let acked_input_tick = self
                .simulation
                .last_input_tick(client.player_id)
                .unwrap_or(0);
            self.send(
                addr,
                Message::Snapshot {
                    server_tick,
                    acked_input_tick,
                    players: players.clone(),
                    loot: loot.clone(),
                    corpses: corpses.clone(),
                    bolts: bolts.clone(),
                    events: events.clone(),
                },
            );
        }

        self.send_net_updates(updates);
    }

    /// Подставляет имена жертв в трупы (история имён переживает отключение).
    fn fill_corpse_names(&self, corpses: &mut [CorpseNet]) {
        for corpse in corpses {
            if corpse.name.is_empty() {
                if let Some(name) = self.names.get(&corpse.owner) {
                    corpse.name = name.clone();
                }
            }
        }
    }

    /// Персональные апдейты: состояние инвентаря владельцу, содержимое
    /// открытого контейнера — открывшему его игроку.
    fn send_net_updates(&mut self, updates: NetUpdates) {
        for (player_id, slots) in updates.inventory {
            if let Some(addr) = self.addr_of(player_id) {
                self.send(addr, Message::InventoryUpdate { slots });
            }
        }
        for (player_id, update) in updates.containers {
            if let Some(addr) = self.addr_of(player_id) {
                self.send(
                    addr,
                    Message::ContainerUpdate {
                        container: update.container,
                        items: update.items,
                    },
                );
            }
        }
    }

    fn addr_of(&self, player_id: u64) -> Option<SocketAddr> {
        self.clients
            .iter()
            .find(|(_, client)| client.player_id == player_id)
            .map(|(addr, _)| *addr)
    }

    /// Подставляет имена в события смертей: симуляция знает только id, а
    /// клиентам нужен килл-фид.
    fn enrich_events(&self, events: Vec<GameEvent>) -> Vec<GameEvent> {
        events
            .into_iter()
            .map(|event| match event {
                GameEvent::Death { victim, killer, .. } => GameEvent::Death {
                    victim,
                    victim_name: self.names.get(&victim).cloned().unwrap_or_default(),
                    killer,
                    // killer = 0 — смерть без игрока-убийцы (аномалия).
                    killer_name: if killer == 0 {
                        "Аномалия".to_string()
                    } else {
                        self.names.get(&killer).cloned().unwrap_or_default()
                    },
                },
                other => other,
            })
            .collect()
    }

    fn check_timeouts(&mut self) {
        let timed_out: Vec<SocketAddr> = self
            .clients
            .iter()
            .filter(|(_, client)| self.now.saturating_sub(client.last_seen) > SERVER_TIMEOUT)
            .map(|(addr, _)| *addr)
            .collect();
        for addr in timed_out {
            self.remove_client(addr, "таймаут");
        }
    }

    fn remove_client(&mut self, addr: SocketAddr, reason: &str) {
        if let Some(client) = self.clients.remove(&addr) {
            self.simulation.despawn_player(client.player_id);
            log::info!(
                "игрок '{}' (id {}) удалён: {reason}, в сессии {}",
                client.name,
                client.player_id,
                self.clients.len()
            );
        }
    }

    fn send(&mut self, addr: SocketAddr, message: Message) {
        let kind = message.kind();
        let bytes = match encode_message(&message) {
            Ok(bytes) => bytes,
            Err(error) => {
                log::error!("{kind} не закодирован: {error}");
                return;
            }
        };

        if self.should_drop_outgoing() {
            self.stats.dropped += 1;
            log::debug!("[диагностика] {kind} для {addr} потерян");
            return;
        }

        if self.config.out_latency.is_zero() {
            self.send_now(addr, &bytes, kind);
        } else {
            self.outbox
                .push_back((self.now + self.config.out_latency, addr, bytes));
        }
    }

    fn send_now(&mut self, addr: SocketAddr, bytes: &[u8], kind: &'static str) {
        match self.socket.send_to(bytes, addr) {
            Ok(_) => self.stats.sent += 1,
            Err(error) => log::debug!("{kind} для {addr} не отправлен: {error}"),
        }
    }

    fn flush_outbox(&mut self) {
        while let Some((send_at, _, _)) = self.outbox.front() {
            if *send_at > self.now {
                break;
            }
            let (_, addr, bytes) = self.outbox.pop_front().expect("проверено выше");
            self.send_now(addr, &bytes, "отложенный пакет");
        }
    }

    fn should_drop_outgoing(&mut self) -> bool {
        let pct = self.config.out_loss_pct.clamp(0.0, 100.0);
        if pct <= 0.0 {
            return false;
        }
        // xorshift32: детерминированная последовательность для smoke-прогонов.
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x >> 8) as f32 / 16_777_216.0 * 100.0 < pct
    }
}

/// Держит инпуты в разумных границах: NaN/бесконечности → 0, оси [-1, 1],
/// углы за тик не больше 90°. Не анти-чит, а защита предсказуемости движения.
fn sanitize_input(input: &mut ClientInput) {
    let clamp_unit = |value: f32| {
        if value.is_finite() {
            value.clamp(-1.0, 1.0)
        } else {
            0.0
        }
    };
    let clamp_look = |value: f32| {
        if value.is_finite() {
            value.clamp(-std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2)
        } else {
            0.0
        }
    };
    input.move_forward = clamp_unit(input.move_forward);
    input.move_right = clamp_unit(input.move_right);
    input.yaw_delta = clamp_look(input.yaw_delta);
    input.pitch_delta = clamp_look(input.pitch_delta);
}

/// Обрезает имя до `MAX_NAME_LEN` байт по границе символа.
fn clamp_name(name: &str) -> String {
    if name.len() <= MAX_NAME_LEN {
        return name.to_string();
    }
    let mut end = MAX_NAME_LEN;
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Каталог предметов из контента репозитория (cwd теста — crate root).
    fn test_catalog() -> ItemCatalog {
        ItemCatalog::load_ron("../../game/content/items/items.ron").expect("каталог предметов")
    }

    #[test]
    fn sanitize_clamps_and_defuses_input() {
        let mut input = ClientInput {
            move_forward: 17.0,
            move_right: -3.5,
            yaw_delta: f32::NAN,
            pitch_delta: 9.0,
            ..ClientInput::new(1)
        };
        sanitize_input(&mut input);
        assert_eq!(input.move_forward, 1.0);
        assert_eq!(input.move_right, -1.0);
        assert_eq!(input.yaw_delta, 0.0);
        assert!((input.pitch_delta - std::f32::consts::FRAC_PI_2).abs() < 1e-6);

        let mut input = ClientInput {
            move_forward: f32::INFINITY,
            pitch_delta: f32::NEG_INFINITY,
            ..ClientInput::new(2)
        };
        sanitize_input(&mut input);
        assert_eq!(input.move_forward, 0.0);
        assert_eq!(input.pitch_delta, 0.0);
    }

    #[test]
    fn name_clamps_to_byte_budget_on_char_boundary() {
        let long = "я".repeat(64); // 128 байт
        let clamped = clamp_name(&long);
        assert!(clamped.len() <= MAX_NAME_LEN);
        assert_eq!(clamped, "я".repeat(16)); // 32 байта

        let short = "сталкер";
        assert_eq!(clamp_name(short), short);
    }

    #[test]
    fn loss_rng_is_deterministic_per_seed() {
        let scene = Scene::demo_zone();
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let config = SessionConfig {
            out_loss_pct: 50.0,
            rng_seed: 12345,
            ..SessionConfig::default()
        };
        let mut session = Session::new(socket, &scene, test_catalog(), config).unwrap();
        let rolls_a: Vec<bool> = (0..16).map(|_| session.should_drop_outgoing()).collect();
        let mut session = Session::new(
            UdpSocket::bind("127.0.0.1:0").unwrap(),
            &scene,
            test_catalog(),
            SessionConfig {
                out_loss_pct: 50.0,
                rng_seed: 12345,
                ..SessionConfig::default()
            },
        )
        .unwrap();
        let rolls_b: Vec<bool> = (0..16).map(|_| session.should_drop_outgoing()).collect();
        assert_eq!(rolls_a, rolls_b);
        assert!(
            rolls_a.iter().any(|drop| *drop),
            "при 50% потерь должны быть дропы"
        );
    }
}
