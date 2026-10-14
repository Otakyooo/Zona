use std::io::{BufRead, Write};
use std::sync::mpsc::channel;
use std::thread;
use std::time::{Duration, Instant};

use glam::Vec3;
use zn_net::{ClientInput, MAX_PLAYERS_PER_SESSION, PROTOCOL_VERSION};
use zn_sim::Simulation;

#[derive(Clone, Copy, Debug)]
struct ServerArgs {
    port: u16,
    max_players: usize,
    tick_hz: u32,
}

impl Default for ServerArgs {
    fn default() -> Self {
        Self {
            port: 7700,
            max_players: MAX_PLAYERS_PER_SESSION,
            tick_hz: 60,
        }
    }
}

fn parse_args() -> Result<ServerArgs, String> {
    let mut args = ServerArgs::default();
    let mut iter = std::env::args().skip(1);
    while let Some(flag) = iter.next() {
        let value = iter
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--port" => {
                args.port = value
                    .parse()
                    .map_err(|_| format!("invalid port: {value}"))?
            }
            "--max-players" => {
                args.max_players = value
                    .parse()
                    .map_err(|_| format!("invalid max players: {value}"))?
            }
            "--tick-hz" => {
                args.tick_hz = value
                    .parse()
                    .map_err(|_| format!("invalid tick hz: {value}"))?
            }
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    args.max_players = args.max_players.clamp(1, MAX_PLAYERS_PER_SESSION * 10);
    args.tick_hz = args.tick_hz.clamp(10, 240);
    Ok(args)
}

fn main() {
    env_logger::init();

    let args = match parse_args() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!("usage: zn-server [--port <port>] [--max-players <n>] [--tick-hz <n>]");
            std::process::exit(2);
        }
    };

    let mut simulation = Simulation::new();

    log::info!(
        "ZONA dedicated server starting: port={} max_players={} tick_hz={} (simulation: {} Hz) protocol={}",
        args.port,
        args.max_players,
        args.tick_hz,
        simulation.tick_hz(),
        PROTOCOL_VERSION
    );

    let (tx, rx) = channel::<String>();

    thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines().map_while(Result::ok) {
            if tx.send(line.trim().to_string()).is_err() {
                break;
            }
        }
    });

    let boot = Instant::now();
    let mut last = Instant::now();
    let mut last_status = Instant::now();
    let mut running = true;

    while running {
        let now = Instant::now();
        let frame_delta = now - last;
        last = now;

        let steps = simulation.advance(frame_delta);
        if steps > 0 {
            log::debug!(target: "zn_server", "ran {steps} fixed steps, tick={}", simulation.tick());
        }

        while let Ok(line) = rx.try_recv() {
            match line.as_str() {
                "quit" | "exit" => running = false,
                "status" | "" => print_status(&mut simulation, args, boot),
                "spawn" => {
                    let id = simulation.spawn_player(Vec3::ZERO);
                    simulation.queue_input(id, ClientInput::new(simulation.tick()));
                    println!("spawned player {id}");
                }
                other => println!("unknown command: {other} (try: status, spawn, quit)"),
            }
        }

        if last_status.elapsed() >= Duration::from_secs(5) {
            last_status = Instant::now();
            print_status(&mut simulation, args, boot);
        }

        thread::sleep(Duration::from_millis(2));
    }

    println!("server stopped after {} ticks", simulation.tick());
    let _ = std::io::stdout().flush();
}

fn print_status(simulation: &mut Simulation, args: ServerArgs, boot: Instant) {
    println!(
        "status: uptime={:?} tick={} players={}/{} snapshot_entities={} protocol={}",
        boot.elapsed(),
        simulation.tick(),
        simulation.player_count(),
        args.max_players,
        simulation.snapshot().len(),
        PROTOCOL_VERSION
    );
    let _ = std::io::stdout().flush();
}
