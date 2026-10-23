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
fn inventory_actions_roundtrip_through_bincode() {
    let actions = [
        InventoryAction::TakeFrom {
            container: 3,
            index: 1,
        },
        InventoryAction::TakeAll { container: 3 },
        InventoryAction::Drop { slot: 39 },
        InventoryAction::Use { slot: 12 },
    ];
    for action in actions {
        let bytes = encode_message(&action).unwrap();
        let decoded: InventoryAction = decode_message(&bytes).unwrap();
        assert_eq!(decoded, action);
    }
}

#[test]
fn stash_actions_roundtrip_through_bincode() {
    let actions = [
        StashAction::Take { index: 0 },
        StashAction::Put { slot: 39 },
    ];
    for action in actions {
        let bytes = encode_message(&action).unwrap();
        let decoded: StashAction = decode_message(&bytes).unwrap();
        assert_eq!(decoded, action);
    }
}

#[test]
fn base_actions_roundtrip_through_bincode() {
    let actions = [
        BaseActionKind::Sell {
            from_stash: true,
            slot: 2,
        },
        BaseActionKind::Sell {
            from_stash: false,
            slot: 39,
        },
        BaseActionKind::Buy { offer: 0 },
        BaseActionKind::Upgrade,
    ];
    for action in actions {
        let bytes = encode_message(&action).unwrap();
        let decoded: BaseActionKind = decode_message(&bytes).unwrap();
        assert_eq!(decoded, action);
    }
}

#[test]
fn all_message_variants_roundtrip_through_bincode() {
    let messages = vec![
        Message::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            content_hash: 0xDEAD_BEEF_CAFE_F00D,
            items_hash: 0x0BAD_F00D_1234_5678,
            name: "Сталкер".to_string(),
            auth: Some(AuthHello {
                account: "stalker-1".to_string(),
                password_digest: password_digest("hunter2"),
            }),
        },
        Message::InputPacket {
            inputs: vec![ClientInput::new(1), ClientInput::new(2)],
        },
        Message::Interact { tick: 41 },
        Message::InventoryAction {
            tick: 42,
            action: InventoryAction::TakeFrom {
                container: 7,
                index: 0,
            },
        },
        Message::ThrowBolt { tick: 43 },
        Message::StashAction {
            tick: 44,
            action: StashAction::Take { index: 2 },
        },
        Message::BaseInteract {
            tick: 45,
            station: 0,
        },
        Message::BaseAction {
            tick: 46,
            action: BaseActionKind::Sell {
                from_stash: true,
                slot: 1,
            },
        },
        Message::BaseAction {
            tick: 47,
            action: BaseActionKind::Buy { offer: 2 },
        },
        Message::BaseAction {
            tick: 48,
            action: BaseActionKind::Upgrade,
        },
        Message::Ping { nonce: 9 },
        Message::ClientBye,
        Message::ServerWelcome {
            player_id: 3,
            server_tick: 120,
            spawn: [0.0, 0.0, 14.0],
            location: 0,
            account_created: false,
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
        Message::ServerReject {
            reason: RejectReason::AuthFailed,
        },
        Message::Snapshot {
            server_tick: 50,
            acked_input_tick: 44,
            players: vec![state(1), state(2)],
            loot: vec![GroundItemNet {
                item: 4,
                count: 30,
                pos: [10, -2, 140],
            }],
            corpses: vec![CorpseNet {
                container: 1 << 62,
                owner: 2,
                name: "Жертва".to_string(),
                pos: [-1, -2, 140],
                yaw: 200,
            }],
            bolts: vec![BoltNet {
                id: 7,
                owner: 1,
                pos: [150, 30, 150],
            }],
            bubbles: vec![[300, 0, -300], [120, 0, 180]],
            mutants: vec![MutantNet {
                id: 1,
                kind: 0,
                pos: [100, 0, 100],
                yaw: 128,
                health: 60,
                state: 1,
            }],
            events: vec![
                GameEvent::Shot {
                    shooter: 1,
                    origin: [0.0, 1.62, 0.0],
                    dir: [1.0, 0.0, 0.0],
                },
                GameEvent::Hit {
                    shooter: 1,
                    victim: 2,
                    damage: 50,
                    headshot: true,
                },
                GameEvent::UseStarted {
                    player: 2,
                    ticks: 120,
                },
                GameEvent::AnomalyRevealed {
                    anomaly: 1,
                    position: [15.0, 0.0, 15.0],
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
        Message::InventoryUpdate {
            slots: vec![
                Some(ItemStackNet { item: 4, count: 3 }),
                None,
                Some(ItemStackNet { item: 0, count: 60 }),
            ],
        },
        Message::ContainerUpdate {
            container: 2,
            items: vec![ItemStackNet { item: 0, count: 60 }],
        },
        Message::LocationChanged {
            location: 3,
            position: [1.0, 0.0, -15.0],
            yaw: 1.5,
        },
        Message::StashUpdate {
            items: vec![ItemStackNet { item: 6, count: 5 }],
        },
        Message::BaseUpdate {
            station: Some(0),
            wallet: 200,
            facilities: [1, 0, 0],
            offers: vec![
                ItemStackNet { item: 1, count: 1 },
                ItemStackNet { item: 4, count: 30 },
            ],
        },
        Message::BaseUpdate {
            station: None,
            wallet: 1_250,
            facilities: [2, 1, 0],
            offers: vec![],
        },
        Message::BaseNotice {
            text: "не хватает RU".to_string(),
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
    let items = 0x0FED_CBA9_8765_4321;
    assert_eq!(
        check_client_hello(PROTOCOL_VERSION, hash, items, hash, items),
        Ok(())
    );

    assert_eq!(
        check_client_hello(PROTOCOL_VERSION + 1, hash, items, hash, items),
        Err(RejectReason::VersionMismatch {
            server: PROTOCOL_VERSION
        })
    );
    assert_eq!(
        check_client_hello(PROTOCOL_VERSION, hash + 1, items, hash, items),
        Err(RejectReason::ContentMismatch)
    );
    assert_eq!(
        check_client_hello(PROTOCOL_VERSION, hash, items + 1, hash, items),
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
        // Худший реалистичный случай: лимит предметов на земле, трупы и
        // события; имена — кириллица.
        loot: (0..24)
            .map(|i| GroundItemNet {
                item: i as u16,
                count: 60,
                pos: [i as i16 * 10, -2, 140],
            })
            .collect(),
        corpses: (1..=4)
            .map(|i| CorpseNet {
                container: (1 << 62) + i,
                owner: i,
                name: "Охотник".to_string(),
                pos: [i as i16, -2, 140],
                yaw: 200,
            })
            .collect(),
        // Лимит болтов в полёте (`BOLTS_MAX` в zn-sim) — 12.
        bolts: (0..12)
            .map(|i| BoltNet {
                id: i,
                owner: u64::from(i % 3),
                pos: [i as i16 * 10, 30, 150],
            })
            .collect(),
        // Максимум активных пузырей на локацию (`BUBBLE_COUNT_MAX`) — 3.
        bubbles: vec![[120, 0, 300], [-300, 0, -40], [260, 0, -340]],
        // Стая мутантов на локации (до 4 особей).
        mutants: (0..4)
            .map(|i| MutantNet {
                id: i,
                kind: 0,
                pos: [i as i16 * 15, 0, 100],
                yaw: 120,
                health: 50,
                state: 1,
            })
            .collect(),
        events: vec![
            GameEvent::Shot {
                shooter: 3,
                origin: [0.0, 1.62, 14.0],
                dir: [1.0, 0.0, 0.0],
            },
            GameEvent::UseStarted {
                player: 5,
                ticks: 120,
            },
            GameEvent::AnomalyRevealed {
                anomaly: 2,
                position: [-15.0, 0.0, 15.0],
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
    // Ethernet MTU 1500 − 20 IP − 8 UDP = 1472: держим запас на будущие поля.
    assert!(
        bytes.len() <= 1400,
        "снапшот на {} игроков занял {} байт",
        MAX_PLAYERS_PER_SESSION,
        bytes.len()
    );
}

#[test]
fn position_and_yaw_quantization_roundtrip() {
    for value in [0.0, 1.5, -0.2, 14.0, 123.4, -100.0] {
        let quantized = quantize_pos([value, 0.0, 0.0]);
        let restored = dequantize_pos(quantized)[0];
        assert!(
            (restored - value).abs() <= POS_QUANTUM / 2.0 + 1e-3,
            "{value} → {restored}"
        );
    }
    assert_eq!(quantize_pos([14.0, -0.2, 0.0]), [140, -2, 0]);

    assert_eq!(quantize_yaw(0.0), 0);
    assert_eq!(quantize_yaw(std::f32::consts::PI), 128);
    let restored = dequantize_yaw(quantize_yaw(-0.1));
    let expected = (-0.1f32).rem_euclid(std::f32::consts::TAU);
    assert!(
        (restored - expected).abs() <= YAW_QUANTUM / 2.0 + 1e-3,
        "{expected} → {restored}"
    );
}

#[test]
fn protocol_constants_are_sane() {
    assert_eq!(PROTOCOL_VERSION, 10);
    assert_eq!(SIMULATION_HZ, 60);
    assert_eq!(SNAPSHOT_HZ, 30);
    assert_eq!(MAX_PLAYERS_PER_SESSION, 10);
    assert_eq!(INPUT_REDUNDANCY, 4);
    assert_eq!(HELLO_RESEND_INTERVAL, std::time::Duration::from_millis(500));
    assert_eq!(PING_INTERVAL, std::time::Duration::from_secs(1));
    assert_eq!(SERVER_TIMEOUT, std::time::Duration::from_secs(5));
    assert_eq!(CLIENT_TIMEOUT, std::time::Duration::from_secs(3));
}

#[test]
fn password_digest_is_deterministic_and_salted_by_password() {
    let a = password_digest("zona-2026");
    let b = password_digest("zona-2026");
    let c = password_digest("Zona-2026");
    assert_eq!(a, b, "один пароль — один дайджест");
    assert_ne!(a, c, "разные пароли — разные дайджесты");
    // SHA-256("") — эталонный вектор: детерминизм функции.
    assert_eq!(
        password_digest(""),
        [
            0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
            0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
            0x78, 0x52, 0xb8, 0x55,
        ]
    );
}
