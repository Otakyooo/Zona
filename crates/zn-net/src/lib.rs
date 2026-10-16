use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Версия протокола. v2 — сетевое движение (M3): handshake, input-пакеты с
/// redundancy, персональные снапшоты, ping; v3 — бой (M4a): здоровье в
/// состояниях, события выстрелов/смертей. Несовпадение версий отвергается.
pub const PROTOCOL_VERSION: u32 = 3;
pub const SIMULATION_HZ: u32 = 60;
pub const SNAPSHOT_HZ: u32 = 30;
pub const MAX_PLAYERS_PER_SESSION: usize = 10;

/// Сколько последних инпутов клиент повторяет в каждом `InputPacket`.
/// Потеря датаграммы не должна превращаться в потерю движения.
pub const INPUT_REDUNDANCY: usize = 4;

/// Максимальная длина имени игрока (байты UTF-8): защита от раздутых Hello.
pub const MAX_NAME_LEN: usize = 32;

/// Клиент повторяет `ClientHello` с этим интервалом, пока нет ответа сервера.
pub const HELLO_RESEND_INTERVAL: Duration = Duration::from_millis(500);
/// Период `Ping` клиента (RTT для заголовка окна/диагностики).
pub const PING_INTERVAL: Duration = Duration::from_secs(1);
/// Сервер: столько без пакетов от клиента — таймаут и `despawn_player`.
pub const SERVER_TIMEOUT: Duration = Duration::from_secs(5);
/// Клиент: столько без снапшотов — «connection lost», возврат в offline.
pub const CLIENT_TIMEOUT: Duration = Duration::from_secs(3);

/// Команда движения за один фиксированный шаг. Одна структура живёт и в
/// offline-клиенте, и в сети (снапшот-реплей), и на сервере.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClientInput {
    pub tick: u64,
    pub move_forward: f32,
    pub move_right: f32,
    pub yaw_delta: f32,
    pub pitch_delta: f32,
    pub jump: bool,
    pub buttons: u32,
}

impl ClientInput {
    pub fn new(tick: u64) -> Self {
        Self {
            tick,
            ..Self::default()
        }
    }
}

/// Каноническое сетевое состояние игрока. `position` — ступни (центр капсулы
/// внутри физики — `PhysicsWorld::character_origin`), `velocity` нужна для
/// точного реплея инпутов после снапа и интерполяции удалённых игроков.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlayerNetState {
    pub player_id: u64,
    pub position: [f32; 3],
    pub velocity: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    /// Здоровье (0 — мёртв до респавна; максимум — `zn_sim::MAX_HEALTH`).
    pub health: u16,
    pub flags: u32,
}

pub const FLAG_GROUNDED: u32 = 1 << 0;
pub const FLAG_SPRINTING: u32 = 1 << 1;
/// Удержание огня: бит `ClientInput.buttons`; в `PlayerNetState.flags` не
/// используется.
pub const FLAG_FIRE: u32 = 1 << 2;

/// События матча, накопленные между снапшотами: визуальная обратная связь и
/// килл-фид. Игровая логика уже применена в симуляции — события нужны только
/// для представления.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum GameEvent {
    /// Выстрел; клиенты рисуют трейсер и вспышку.
    Shot {
        shooter: u64,
        origin: [f32; 3],
        dir: [f32; 3],
    },
    /// Смерть игрока (имена заполняет серверная сессия).
    Death {
        victim: u64,
        victim_name: String,
        killer: u64,
        killer_name: String,
    },
}

/// Причина отказа в подключении (`ServerReject`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectReason {
    /// Несовпадение версии протокола; `server` — версия сервера.
    VersionMismatch { server: u32 },
    /// `Scene::content_hash()` клиента не совпал с серверным.
    ContentMismatch,
    /// В сессии нет свободных слотов.
    SessionFull,
}

/// Одна датаграмма = одно сообщение. Все пакеты самодостаточны: потеря
/// датаграммы не ломает состояние, следующий пакет восстанавливает картину.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Message {
    // ── клиент → сервер ─────────────────────────────────────────────────
    /// Представление; повторяется каждые `HELLO_RESEND_INTERVAL` до ответа.
    ClientHello {
        protocol_version: u32,
        content_hash: u64,
        name: String,
    },
    /// Последние инпуты (не более `INPUT_REDUNDANCY`, по возрастанию tick).
    InputPacket {
        inputs: Vec<ClientInput>,
    },
    Ping {
        nonce: u32,
    },
    ClientBye,

    // ── сервер → клиент ─────────────────────────────────────────────────
    ServerWelcome {
        player_id: u64,
        server_tick: u64,
        /// Точка спавна уровня (ступни).
        spawn: [f32; 3],
    },
    ServerReject {
        reason: RejectReason,
    },
    /// Персональный снапшот (ack зависит от адресата), период `SNAPSHOT_HZ`.
    Snapshot {
        server_tick: u64,
        /// Максимальный tick инпута, учтённого в этом снапшоте (reconciliation).
        acked_input_tick: u64,
        players: Vec<PlayerNetState>,
        /// События с прошлого снапшота (выстрелы, смерти) — всем клиентам.
        events: Vec<GameEvent>,
    },
    Pong {
        nonce: u32,
        server_tick: u64,
    },
}

impl Message {
    /// Короткое имя варианта для логов и диагностики.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ClientHello { .. } => "ClientHello",
            Self::InputPacket { .. } => "InputPacket",
            Self::Ping { .. } => "Ping",
            Self::ClientBye => "ClientBye",
            Self::ServerWelcome { .. } => "ServerWelcome",
            Self::ServerReject { .. } => "ServerReject",
            Self::Snapshot { .. } => "Snapshot",
            Self::Pong { .. } => "Pong",
        }
    }
}

/// Проверка `ClientHello` на стороне сервера. `Ok(())` — можно принимать:
/// версия совпадает и хэш загруженного уровня тот же (иначе коллайдеры
/// клиента и сервера разойдутся).
pub fn check_client_hello(
    protocol_version: u32,
    content_hash: u64,
    expected_content_hash: u64,
) -> Result<(), RejectReason> {
    if protocol_version != PROTOCOL_VERSION {
        return Err(RejectReason::VersionMismatch {
            server: PROTOCOL_VERSION,
        });
    }
    if content_hash != expected_content_hash {
        return Err(RejectReason::ContentMismatch);
    }
    Ok(())
}

#[derive(Debug)]
pub enum NetError {
    Encode(String),
    Decode(String),
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Encode(e) => write!(f, "encode error: {e}"),
            Self::Decode(e) => write!(f, "decode error: {e}"),
        }
    }
}

impl std::error::Error for NetError {}

pub fn encode_message<T: Serialize>(message: &T) -> Result<Vec<u8>, NetError> {
    bincode::serialize(message).map_err(|e| NetError::Encode(e.to_string()))
}

pub fn decode_message<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, NetError> {
    bincode::deserialize(bytes).map_err(|e| NetError::Decode(e.to_string()))
}
