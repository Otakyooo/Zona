use zn_net::*;

#[test]
fn input_roundtrips_through_bincode() {
    let input = ClientInput {
        tick: 42,
        move_forward: 1.0,
        move_right: -0.5,
        yaw_delta: 0.1,
        pitch_delta: -0.2,
        jump: true,
        buttons: 3,
    };
    let bytes = encode_message(&input).unwrap();
    let decoded: ClientInput = decode_message(&bytes).unwrap();
    assert_eq!(decoded, input);
}

#[test]
fn snapshot_roundtrips_through_bincode() {
    let snapshot = PlayerSnapshot {
        entity_id: 7,
        position: [1.0, 2.0, 3.0],
        yaw: 0.5,
        pitch: -0.25,
        flags: FLAG_GROUNDED,
    };
    let bytes = encode_message(&snapshot).unwrap();
    let decoded: PlayerSnapshot = decode_message(&bytes).unwrap();
    assert_eq!(decoded, snapshot);
}

#[test]
fn garbage_payload_is_rejected() {
    let result: Result<ClientInput, _> = decode_message(&[0xff; 3]);
    assert!(result.is_err());
}

#[test]
fn protocol_constants_are_sane() {
    assert_eq!(SIMULATION_HZ, 60);
    assert_eq!(SNAPSHOT_HZ, 30);
    assert_eq!(MAX_PLAYERS_PER_SESSION, 10);
}
