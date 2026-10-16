//! Сетевой клиент: non-blocking UDP в том же потоке (без net-потоков).
//! `poll(now)` обрабатывает входящие пакеты и таймеры исходящих (Hello, Ping);
//! всё время приходит параметром — e2e-тесты гоняют виртуальные `Instant`
//! без sleep-ов.

use std::io;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::Instant;

use zn_net::{
    decode_message, encode_message, ClientInput, GameEvent, Message, PlayerNetState, RejectReason,
    CLIENT_TIMEOUT, HELLO_RESEND_INTERVAL, INPUT_REDUNDANCY, PING_INTERVAL, PROTOCOL_VERSION,
};

/// События для приложения, извлечённые из входящих пакетов.
#[derive(Clone, Debug, PartialEq)]
pub enum NetEvent {
    Welcome {
        player_id: u64,
        server_tick: u64,
        spawn: [f32; 3],
    },
    Reject(RejectReason),
    Snapshot {
        server_tick: u64,
        acked_input_tick: u64,
        players: Vec<PlayerNetState>,
        /// События боя с прошлого снапшота (трейсеры, килл-фид).
        events: Vec<GameEvent>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetStatus {
    /// Hello отправлен, ждём Welcome.
    Connecting,
    /// Welcome получен, идёт обмен инпутами/снапшотами.
    Connected,
    /// Сервер отказал; приложение решает, продолжать ли offline.
    Rejected(RejectReason),
}

pub struct NetClient {
    socket: UdpSocket,
    server: SocketAddr,
    content_hash: u64,
    name: String,
    status: NetStatus,
    last_hello: Option<Instant>,
    last_ping: Option<Instant>,
    last_snapshot: Option<Instant>,
    pending_ping: Option<(u32, Instant)>,
    rtt_ms: Option<f32>,
    next_nonce: u32,
    last_sent_tick: Option<u64>,
    /// Уже сообщали в лог, что сервер не отвечает (серия UDP-сбросов).
    server_unreachable: bool,
}

impl NetClient {
    /// Подключается к серверу (UDP `connect` на первый разрешённый адрес).
    pub fn connect(server: &str, content_hash: u64, name: &str) -> io::Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        socket.set_nonblocking(true)?;
        let addr = server
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "пустой адрес сервера"))?;
        socket.connect(addr)?;
        Ok(Self {
            socket,
            server: addr,
            content_hash,
            name: name.to_string(),
            status: NetStatus::Connecting,
            last_hello: None,
            last_ping: None,
            last_snapshot: None,
            pending_ping: None,
            rtt_ms: None,
            next_nonce: 1,
            last_sent_tick: None,
            server_unreachable: false,
        })
    }

    pub fn status(&self) -> NetStatus {
        self.status
    }

    pub fn rtt_ms(&self) -> Option<f32> {
        self.rtt_ms
    }

    /// Снапшотов не было дольше `CLIENT_TIMEOUT` — связь потеряна.
    pub fn timed_out(&self, now: Instant) -> bool {
        self.status == NetStatus::Connected
            && self
                .last_snapshot
                .is_some_and(|last| now.duration_since(last) > CLIENT_TIMEOUT)
    }

    /// Прощается с сервером (best-effort при выходе из игры).
    pub fn send_bye(&mut self) {
        match self.status {
            NetStatus::Connecting | NetStatus::Connected => self.send(&Message::ClientBye),
            NetStatus::Rejected(_) => {}
        }
    }

    /// Обрабатывает таймеры и входящие пакеты. Вызывается в начале кадра.
    pub fn poll(&mut self, now: Instant) -> Vec<NetEvent> {
        self.poll_timers(now);
        self.drain_socket(now)
    }

    /// Отправляет последние инпуты (не более `INPUT_REDUNDANCY`) в каждом кадре
    /// с новыми шагами. Работает только в состоянии `Connected`.
    pub fn send_inputs(&mut self, inputs: &[ClientInput]) {
        if self.status != NetStatus::Connected {
            return;
        }
        let skip = inputs.len().saturating_sub(INPUT_REDUNDANCY);
        let recent = &inputs[skip..];
        let Some(last) = recent.last() else {
            return;
        };
        if self.last_sent_tick == Some(last.tick) {
            return;
        }
        self.send(&Message::InputPacket {
            inputs: recent.to_vec(),
        });
        self.last_sent_tick = Some(last.tick);
    }

    fn poll_timers(&mut self, now: Instant) {
        match self.status {
            NetStatus::Connecting => {
                let due = self
                    .last_hello
                    .is_none_or(|last| now.duration_since(last) >= HELLO_RESEND_INTERVAL);
                if due {
                    self.last_hello = Some(now);
                    let hello = Message::ClientHello {
                        protocol_version: PROTOCOL_VERSION,
                        content_hash: self.content_hash,
                        name: self.name.clone(),
                    };
                    self.send(&hello);
                }
            }
            NetStatus::Connected => {
                let due = self
                    .last_ping
                    .is_none_or(|last| now.duration_since(last) >= PING_INTERVAL);
                if due {
                    self.last_ping = Some(now);
                    let nonce = self.next_nonce;
                    self.next_nonce = self.next_nonce.wrapping_add(1);
                    self.pending_ping = Some((nonce, now));
                    self.send(&Message::Ping { nonce });
                }
            }
            NetStatus::Rejected(_) => {}
        }
    }

    fn drain_socket(&mut self, now: Instant) -> Vec<NetEvent> {
        let mut events = Vec::new();
        let mut buf = [0u8; 2048];
        loop {
            match self.socket.recv(&mut buf) {
                Ok(len) => {
                    // Сервер снова отвечает — серия сбросов прервана.
                    self.server_unreachable = false;
                    match decode_message::<Message>(&buf[..len]) {
                        Ok(message) => {
                            if let Some(event) = self.handle_message(message, now) {
                                events.push(event);
                            }
                        }
                        Err(error) => {
                            log::debug!("пакет от {} не декодирован: {error}", self.server);
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    // ICMP «порт недоступен» после падения сервера: на Windows
                    // это WSAECONNRESET на каждый кадр, на Linux — ECONNREFUSED.
                    // Канал не сломан, связь оборвёт обычный таймаут снапшотов,
                    // поэтому пишем в лог один раз на серию ошибок.
                    if !self.server_unreachable {
                        self.server_unreachable = true;
                        log::debug!(
                            "сервер {} не отвечает (UDP-сброс); ждём таймаут связи",
                            self.server
                        );
                    }
                    break;
                }
                Err(error) => {
                    log::debug!("recv от {}: {error}", self.server);
                    break;
                }
            }
        }
        events
    }

    fn handle_message(&mut self, message: Message, now: Instant) -> Option<NetEvent> {
        match message {
            Message::ServerWelcome {
                player_id,
                server_tick,
                spawn,
            } => {
                if self.status == NetStatus::Connecting {
                    log::info!(
                        "сервер {} принял: player_id={player_id}, tick={server_tick}",
                        self.server
                    );
                    self.status = NetStatus::Connected;
                    self.last_snapshot = Some(now); // грейс: 3 с до первого снапшота
                    self.last_ping = Some(now);
                }
                Some(NetEvent::Welcome {
                    player_id,
                    server_tick,
                    spawn,
                })
            }
            Message::ServerReject { reason } => {
                log::warn!("сервер {} отклонил подключение: {reason:?}", self.server);
                self.status = NetStatus::Rejected(reason);
                Some(NetEvent::Reject(reason))
            }
            Message::Snapshot {
                server_tick,
                acked_input_tick,
                players,
                events,
            } => {
                if self.status != NetStatus::Connected {
                    return None;
                }
                self.last_snapshot = Some(now);
                Some(NetEvent::Snapshot {
                    server_tick,
                    acked_input_tick,
                    players,
                    events,
                })
            }
            Message::Pong { nonce, .. } => {
                if let Some((pending_nonce, sent_at)) = self.pending_ping {
                    if pending_nonce == nonce {
                        let sample = now.saturating_duration_since(sent_at).as_secs_f32() * 1000.0;
                        // EMA сглаживает джиттер.
                        self.rtt_ms = Some(match self.rtt_ms {
                            Some(previous) => previous * 0.8 + sample * 0.2,
                            None => sample,
                        });
                        self.pending_ping = None;
                    }
                }
                None
            }
            other => {
                log::debug!("игнорируем {} от {}", other.kind(), self.server);
                None
            }
        }
    }

    fn send(&mut self, message: &Message) {
        match encode_message(message) {
            Ok(bytes) => {
                if let Err(error) = self.socket.send(&bytes) {
                    log::debug!("{} не отправлен {}: {error}", message.kind(), self.server);
                }
            }
            Err(error) => log::error!("{} не закодирован: {error}", message.kind()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Сокет «сервера»: тесты играют за обе стороны в одном потоке.
    fn loopback_server() -> UdpSocket {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        socket
    }

    fn drain(server: &UdpSocket) -> Option<(Message, SocketAddr)> {
        let mut buf = [0u8; 2048];
        match server.recv_from(&mut buf) {
            Ok((len, from)) => Some((decode_message(&buf[..len]).unwrap(), from)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => None,
            Err(error) => panic!("recv_from: {error}"),
        }
    }

    /// Ждёт датаграмму от клиента (loopback доставляет асинхронно).
    fn expect_message(server: &UdpSocket) -> (Message, SocketAddr) {
        for _ in 0..1000 {
            if let Some(message) = drain(server) {
                return message;
            }
            std::thread::yield_now();
        }
        panic!("датаграмма от клиента не пришла");
    }

    fn send_to(server: &UdpSocket, addr: SocketAddr, message: &Message) {
        let bytes = encode_message(message).unwrap();
        server.send_to(&bytes, addr).unwrap();
    }

    /// Клиент в состоянии `Connected`: Hello → Welcome.
    fn connected_client(server: &UdpSocket) -> (NetClient, SocketAddr) {
        let mut client =
            NetClient::connect(&server.local_addr().unwrap().to_string(), 0xAB, "Тест").unwrap();
        client.poll(Instant::now());
        let (message, addr) = expect_message(server);
        assert!(matches!(message, Message::ClientHello { .. }));
        send_to(
            server,
            addr,
            &Message::ServerWelcome {
                player_id: 1,
                server_tick: 10,
                spawn: [0.0, 0.0, 14.0],
            },
        );
        let events = client.poll(Instant::now());
        assert_eq!(events.len(), 1, "ожидался Welcome: {events:?}");
        (client, addr)
    }

    fn input_ticks(message: &Message) -> Vec<u64> {
        match message {
            Message::InputPacket { inputs } => inputs.iter().map(|input| input.tick).collect(),
            other => panic!("ожидался InputPacket, получили {}", other.kind()),
        }
    }

    #[test]
    fn hello_resends_until_welcome() {
        let server = loopback_server();
        let mut client =
            NetClient::connect(&server.local_addr().unwrap().to_string(), 0x42, "Сталкер").unwrap();
        let t0 = Instant::now();

        client.poll(t0);
        let (message, addr) = expect_message(&server);
        match message {
            Message::ClientHello {
                protocol_version,
                content_hash,
                name,
            } => {
                assert_eq!(protocol_version, PROTOCOL_VERSION);
                assert_eq!(content_hash, 0x42);
                assert_eq!(name, "Сталкер");
            }
            other => panic!("ожидался ClientHello, получили {}", other.kind()),
        }

        // До HELLO_RESEND_INTERVAL повтор не отправляется.
        client.poll(t0 + HELLO_RESEND_INTERVAL - Duration::from_millis(100));
        assert!(drain(&server).is_none());

        // После — повтор.
        client.poll(t0 + HELLO_RESEND_INTERVAL + Duration::from_millis(1));
        assert!(matches!(
            expect_message(&server).0,
            Message::ClientHello { .. }
        ));

        // Welcome переводит в Connected: Hello больше не шлётся (Ping ещё не пора).
        send_to(
            &server,
            addr,
            &Message::ServerWelcome {
                player_id: 5,
                server_tick: 7,
                spawn: [1.0, 2.0, 3.0],
            },
        );
        let events = client.poll(t0 + HELLO_RESEND_INTERVAL + Duration::from_millis(2));
        assert_eq!(
            events,
            vec![NetEvent::Welcome {
                player_id: 5,
                server_tick: 7,
                spawn: [1.0, 2.0, 3.0],
            }]
        );
        assert_eq!(client.status(), NetStatus::Connected);

        client.poll(t0 + HELLO_RESEND_INTERVAL * 2);
        assert!(
            drain(&server).is_none(),
            "после Welcome Hello не повторяется"
        );
    }

    #[test]
    fn input_packet_carries_last_inputs_once() {
        let server = loopback_server();
        let (mut client, _addr) = connected_client(&server);

        let inputs: Vec<ClientInput> = (0..6).map(ClientInput::new).collect();
        client.send_inputs(&inputs);
        let (message, _) = expect_message(&server);
        assert_eq!(
            input_ticks(&message),
            vec![2, 3, 4, 5],
            "шлём последние {INPUT_REDUNDANCY}"
        );

        // Тот же последний tick — повторный пакет не нужен.
        client.send_inputs(&inputs);
        assert!(drain(&server).is_none());

        // Новый инпут — пакет уходит снова (новое окно).
        let mut more = inputs;
        more.push(ClientInput::new(6));
        client.send_inputs(&more);
        let (message, _) = expect_message(&server);
        assert_eq!(input_ticks(&message), vec![3, 4, 5, 6]);

        // Прощание доходит до сервера.
        client.send_bye();
        assert!(matches!(expect_message(&server).0, Message::ClientBye));
    }

    #[test]
    fn ping_pong_measures_rtt() {
        let server = loopback_server();
        let (mut client, addr) = connected_client(&server);

        // Проходит PING_INTERVAL — клиент шлёт Ping.
        let sent_at = Instant::now() + PING_INTERVAL;
        assert!(client.poll(sent_at).is_empty());
        let nonce = match expect_message(&server).0 {
            Message::Ping { nonce } => nonce,
            other => panic!("ожидался Ping, получили {}", other.kind()),
        };

        // Ответ через 40 мс — RTT ≈ 40 мс.
        send_to(
            &server,
            addr,
            &Message::Pong {
                nonce,
                server_tick: 3,
            },
        );
        let pong_at = sent_at + Duration::from_millis(40);
        assert!(client.poll(pong_at).is_empty());
        let rtt = client.rtt_ms().expect("RTT должен быть измерен");
        assert!((rtt - 40.0).abs() < 0.5, "rtt = {rtt}");
    }

    #[test]
    fn timeout_after_silence_and_snapshot_resets_it() {
        let server = loopback_server();
        let (mut client, addr) = connected_client(&server);

        let almost = Instant::now() + CLIENT_TIMEOUT - Duration::from_millis(100);
        assert!(!client.timed_out(almost), "до таймаута связь жива");
        let silent = Instant::now() + CLIENT_TIMEOUT + Duration::from_secs(1);
        assert!(
            client.timed_out(silent),
            "молчание дольше CLIENT_TIMEOUT — потеря связи"
        );

        // Пришедший снапшот сбрасывает таймаут.
        send_to(
            &server,
            addr,
            &Message::Snapshot {
                server_tick: 30,
                acked_input_tick: 12,
                players: vec![],
                events: vec![],
            },
        );
        let events = client.poll(silent);
        assert_eq!(
            events,
            vec![NetEvent::Snapshot {
                server_tick: 30,
                acked_input_tick: 12,
                players: vec![],
                events: vec![],
            }]
        );
        assert!(!client.timed_out(silent));
    }

    #[test]
    fn reject_stops_outgoing_traffic() {
        let server = loopback_server();
        let mut client =
            NetClient::connect(&server.local_addr().unwrap().to_string(), 0, "Тест").unwrap();
        let t0 = Instant::now();
        client.poll(t0);
        let (_, addr) = expect_message(&server);

        send_to(
            &server,
            addr,
            &Message::ServerReject {
                reason: RejectReason::ContentMismatch,
            },
        );
        let events = client.poll(t0 + Duration::from_millis(10));
        assert_eq!(
            events,
            vec![NetEvent::Reject(RejectReason::ContentMismatch)]
        );
        assert_eq!(
            client.status(),
            NetStatus::Rejected(RejectReason::ContentMismatch)
        );

        // Ни Hello, ни инпуты, ни прощание наружу не уходят.
        client.send_inputs(&[ClientInput::new(0)]);
        client.send_bye();
        client.poll(t0 + Duration::from_secs(30));
        assert!(drain(&server).is_none());
    }

    #[test]
    fn malformed_datagrams_are_ignored() {
        let server = loopback_server();
        let (mut client, addr) = connected_client(&server);

        server.send_to(&[0xFF; 8], addr).unwrap();
        let events = client.poll(Instant::now());
        assert!(events.is_empty(), "{events:?}");
        assert_eq!(client.status(), NetStatus::Connected);
    }
}
