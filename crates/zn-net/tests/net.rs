use zn_net::*;

fn state(player_id: u64) -> PlayerNetState {
    PlayerNetState {
        player_id,
        position: [1.0, 2.0, 3.0],
        velocity: [0.0, -1.5, 4.0],
        yaw: 0.5,
        pitch: -0.25,
        health: 100,
        flags: FLAG_GROUNDED,
    }
}

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
fn net_state_roundtrips_through_bincode() {
    let net = state(7);
    let bytes = encode_message(&net).unwrap();
    let decoded: PlayerNetState = decode_message(&bytes).unwrap();
    assert_eq!(decoded, net);
}

#[test]
fn all_message_variants_roundtrip_through_bincode() {
    let messages = vec![
        Message::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            content_hash: 0xDEAD_BEEF_CAFE_F00D,
            name: "Сталкер".to_string(),
        },
        Message::InputPacket {
            inputs: vec![ClientInput::new(1), ClientInput::new(2)],
        },
        Message::Ping { nonce: 9 },
        Message::ClientBye,
        Message::ServerWelcome {
            player_id: 3,
            server_tick: 120,
            spawn: [0.0, 0.0, 14.0],
        },
        Message::ServerReject {
            reason: RejectReason::VersionMismatch { server: 99 },
        },
        Message::ServerReject {
            reason: RejectReason::ContentMismatch,
        },
        Message::ServerReject {
            reason: RejectReason::SessionFull,
        },
        Message::Snapshot {
            server_tick: 50,
            acked_input_tick: 44,
            players: vec![state(1), state(2)],
            events: vec![
                GameEvent::Shot {
                    shooter: 1,
                    origin: [0.0, 1.62, 0.0],
                    dir: [1.0, 0.0, 0.0],
                },
                GameEvent::Death {
                    victim: 2,
                    victim_name: "Жертва".to_string(),
                    killer: 1,
                    killer_name: "Сталкер".to_string(),
                },
            ],
        },
        Message::Pong {
            nonce: 9,
            server_tick: 51,
        },
    ];

    for message in messages {
        let bytes = encode_message(&message).unwrap();
        let decoded: Message = decode_message(&bytes).unwrap();
        assert_eq!(decoded, message, "вариант {}", message.kind());
    }
}

#[test]
fn garbage_payload_is_rejected() {
    let result: Result<ClientInput, _> = decode_message(&[0xff; 3]);
    assert!(result.is_err());
}

#[test]
fn truncated_and_garbage_messages_are_rejected() {
    let bytes = encode_message(&Message::Ping { nonce: 7 }).unwrap();
    let truncated: Result<Message, _> = decode_message(&bytes[..bytes.len() - 1]);
    assert!(truncated.is_err(), "усечённый пакет должен отклоняться");

    let garbage: Result<Message, _> = decode_message(&[0xAB; 8]);
    assert!(garbage.is_err(), "мусор должен отклоняться");
}

#[test]
fn hello_validation_rejects_version_and_content_mismatch() {
    let hash = 0x1234_5678_9ABC_DEF0;
    assert_eq!(check_client_hello(PROTOCOL_VERSION, hash, hash), Ok(()));

    assert_eq!(
        check_client_hello(PROTOCOL_VERSION + 1, hash, hash),
        Err(RejectReason::VersionMismatch {
            server: PROTOCOL_VERSION
        })
    );
    assert_eq!(
        check_client_hello(PROTOCOL_VERSION, hash + 1, hash),
        Err(RejectReason::ContentMismatch)
    );
}

#[test]
fn full_snapshot_fits_udp_mtu() {
    let players: Vec<PlayerNetState> = (1..=MAX_PLAYERS_PER_SESSION as u64).map(state).collect();
    let message = Message::Snapshot {
        server_tick: 1000,
        acked_input_tick: 999,
        players,
        // Худший реалистичный случай: снапшот с событиями, имена — кириллица.
        events: vec![
            GameEvent::Shot {
                shooter: 3,
                origin: [0.0, 1.62, 14.0],
                dir: [1.0, 0.0, 0.0],
            },
            GameEvent::Death {
                victim: 5,
                victim_name: "Охотник".to_string(),
                killer: 3,
                killer_name: "Сталкер".to_string(),
            },
        ],
    };
    let bytes = encode_message(&message).unwrap();
    assert!(
        bytes.len() <= 1200,
        "снапшот на {} игроков занял {} байт",
        MAX_PLAYERS_PER_SESSION,
        bytes.len()
    );
}

#[test]
fn protocol_constants_are_sane() {
    assert_eq!(PROTOCOL_VERSION, 3);
    assert_eq!(SIMULATION_HZ, 60);
    assert_eq!(SNAPSHOT_HZ, 30);
    assert_eq!(MAX_PLAYERS_PER_SESSION, 10);
    assert_eq!(INPUT_REDUNDANCY, 4);
    assert_eq!(HELLO_RESEND_INTERVAL, std::time::Duration::from_millis(500));
    assert_eq!(PING_INTERVAL, std::time::Duration::from_secs(1));
    assert_eq!(SERVER_TIMEOUT, std::time::Duration::from_secs(5));
    assert_eq!(CLIENT_TIMEOUT, std::time::Duration::from_secs(3));
}
