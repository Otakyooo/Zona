//! Сетевая диагностика лаунчера (временный инструмент, не поставляется).
//!
//! raw  — голый native-tls (как у ureq), дампит сырые байты ответа;
//! ureq — тот же путь, что и у лаунчера (manifest::agent()).
//!
//! Запуск (desktop-контекст, с токеном):
//!   set ZONA_DIAG_TOKEN=github_pat_...
//!   netdiag raw|ureq [host:port|ip]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

const HOST: &str = "api.github.com";
const PATH: &str = "/repos/Otakyooo/ai_games/releases/tags/dev";

fn token() -> String {
    std::env::var("ZONA_DIAG_TOKEN").unwrap_or_default()
}

fn escape(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        match b {
            0x20..=0x7E => out.push(b as char),
            b => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

fn raw_mode(endpoint: Option<String>) {
    let token = token();
    // Резолв как у ureq: getaddrinfo -> connect (в TUN-режиме отдаст fake-IP).
    let addr = match endpoint {
        Some(a) => a,
        None => format!("{HOST}:443"),
    };
    println!("== raw: connect {addr}");
    let stream = TcpStream::connect(&addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();

    let connector = native_tls::TlsConnector::new().expect("connector");
    // Сидней/SNI: передаём домен (как ureq).
    let mut tls = connector
        .connect(HOST, stream)
        .expect("tls handshake FAILED");

    let mut request = format!(
        "GET {PATH} HTTP/1.1\r\nHost: {HOST}\r\nAccept: application/vnd.github+json\r\nUser-Agent: zn-launcher\r\n"
    );
    if !token.is_empty() {
        request.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    request.push_str("Connection: close\r\n\r\n");
    println!("== raw: request:\n{request}");
    tls.write_all(request.as_bytes()).expect("write");

    let mut buf = vec![0u8; 4096];
    let mut got = Vec::new();
    loop {
        match tls.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                got.extend_from_slice(&buf[..n]);
                if got.len() > 8192 {
                    break;
                }
            }
            Err(e) => {
                println!("== raw: read error after {} bytes: {e}", got.len());
                break;
            }
        }
    }
    println!("== raw: got {} bytes", got.len());
    println!("== raw: escaped:\n{}", escape(&got));
}

fn ureq_mode() {
    let token = std::env::var("ZONA_DIAG_TOKEN").ok();
    let agent = zn_launcher::manifest::agent();
    let url = format!("https://{HOST}{PATH}");
    println!("== ureq: GET {url} (token: {})", if token.is_some() { "да" } else { "нет" });
    let mut request = agent
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "zn-launcher");
    if let Some(token) = &token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    match request.call() {
        Ok(response) => {
            let status = response.status();
            println!("== ureq: status {status}");
            let body = response.into_body().read_to_string().unwrap_or_default();
            println!("== ureq: body {} bytes", body.len());
        }
        Err(error) => println!("== ureq: ERROR: {error}"),
    }
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "raw".into());
    match mode.as_str() {
        "raw" => raw_mode(std::env::args().nth(2)),
        "ureq" => ureq_mode(),
        other => eprintln!("неизвестный режим: {other} (raw|ureq)"),
    }
}
