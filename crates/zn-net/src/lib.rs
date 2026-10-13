use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const SIMULATION_HZ: u32 = 60;
pub const SNAPSHOT_HZ: u32 = 30;
pub const MAX_PLAYERS_PER_SESSION: usize = 10;

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
        Self { tick, ..Self::default() }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlayerSnapshot {
    pub entity_id: u32,
    pub position: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub flags: u32,
}

pub const FLAG_GROUNDED: u32 = 1 << 0;
pub const FLAG_SPRINTING: u32 = 1 << 1;

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
