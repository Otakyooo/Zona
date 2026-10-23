mod app;
mod assets;
mod interp;
mod level;
mod net;
mod prediction;

use app::ClientApp;
use level::{DEFAULT_CONTENT_ROOT, DEFAULT_LEVEL};

use std::path::PathBuf;

const DEFAULT_SERVER_ADDR: &str = "127.0.0.1:7700";

struct Cli {
    content_root: PathBuf,
    /// Имя одиночного уровня; `None` — мир локаций (нужен сервер).
    level: Option<String>,
    server: Option<String>,
}

fn usage() -> String {
    format!(
        "ZONA client\n\n\
         Использование:\n  \
           zn-client [--content-root <путь>] [--level <имя>] [--server host:port] [--offline]\n\n\
         Опции:\n  \
           --content-root <путь>  корень контента (по умолчанию {DEFAULT_CONTENT_ROOT});\n                              \
                                   переменная окружения ZN_CONTENT_ROOT переопределяет\n  \
           --level <имя>          одиночная сцена из <content-root>/levels/<имя>.scene.ron:\n                              \
                                   без --server запускается offline;\n                              \
                                   с сервером без опции загружается мир локаций (locations.ron)\n  \
           --server host:port     подключиться к UDP-серверу (по умолчанию {DEFAULT_SERVER_ADDR});\n                              \
                                   переменная окружения ZN_SERVER_ADDR (пустая — offline)\n  \
           --offline              запуск без сети (offline); если --level не указан, уровень {DEFAULT_LEVEL}\n  \
           -h, --help             эта справка\n"
    )
}

fn parse_args() -> Cli {
    let mut content_root: Option<PathBuf> = None;
    let mut level: Option<String> = None;
    let mut server: Option<String> = None;
    let mut offline = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--content-root" => {
                content_root = args.next().map(PathBuf::from);
                if content_root.is_none() {
                    eprintln!("--content-root требует значение\n{}", usage());
                    std::process::exit(2);
                }
            }
            "--level" => {
                level = args.next();
                if level.is_none() {
                    eprintln!("--level требует значение\n{}", usage());
                    std::process::exit(2);
                }
            }
            "--server" => {
                server = args.next();
                if server.is_none() {
                    eprintln!("--server требует значение host:port\n{}", usage());
                    std::process::exit(2);
                }
            }
            "--offline" => {
                offline = true;
            }
            "-h" | "--help" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            other => {
                eprintln!("неизвестный аргумент: {other}\n{}", usage());
                std::process::exit(2);
            }
        }
    }

    let content_root = content_root
        .or_else(|| std::env::var_os("ZN_CONTENT_ROOT").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CONTENT_ROOT));

    let env_server = std::env::var("ZN_SERVER_ADDR").ok();
    let server = resolve_server(offline, server, env_server, level.as_deref());

    Cli {
        content_root,
        level,
        server,
    }
}

/// Куда подключаться: `--offline` → никуда; затем `--server`, затем
/// `ZN_SERVER_ADDR` (пустая — offline); без них мир локаций идёт на
/// локальный сервер, а `--level` без сервера — одиночная offline-сцена.
fn resolve_server(
    offline: bool,
    explicit: Option<String>,
    env: Option<String>,
    level: Option<&str>,
) -> Option<String> {
    if offline {
        return None;
    }
    if explicit.is_some() {
        return explicit;
    }
    if let Some(addr) = env {
        let addr = addr.trim();
        return (!addr.is_empty()).then(|| addr.to_string());
    }
    level.is_none().then(|| DEFAULT_SERVER_ADDR.to_string())
}

fn main() {
    env_logger::init();
    let cli = parse_args();
    log::info!(
        "ZONA client starting: content-root={}, режим={}, server={}",
        cli.content_root.display(),
        cli.level.as_deref().unwrap_or("<мир локаций>"),
        cli.server.as_deref().unwrap_or("<offline>")
    );

    let event_loop = winit::event_loop::EventLoop::new().expect("failed to create event loop");
    let mut client = match cli.server {
        Some(server) => ClientApp::with_server(server, cli.content_root, cli.level.as_deref()),
        None => {
            let level = cli.level.as_deref().unwrap_or(DEFAULT_LEVEL);
            ClientApp::with_content(cli.content_root, level)
        }
    };
    if let Err(error) = event_loop.run_app(&mut client) {
        log::error!("client terminated with error: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn some(addr: &str) -> Option<String> {
        Some(addr.to_string())
    }

    #[test]
    fn offline_flag_wins_over_everything() {
        let server = resolve_server(true, some("1.2.3.4:7700"), some("5.6.7.8:7700"), None);
        assert_eq!(server, None);
    }

    #[test]
    fn explicit_server_then_env() {
        let server = resolve_server(false, some("1.2.3.4:7700"), some("5.6.7.8:7700"), None);
        assert_eq!(server, some("1.2.3.4:7700"));
        let server = resolve_server(false, None, some(" 5.6.7.8:7700\r\n"), Some("kordon"));
        assert_eq!(server, some("5.6.7.8:7700"));
        assert_eq!(resolve_server(false, None, some("  "), None), None);
    }

    #[test]
    fn world_defaults_to_local_server_and_level_to_offline() {
        assert_eq!(
            resolve_server(false, None, None, None),
            some(DEFAULT_SERVER_ADDR)
        );
        assert_eq!(resolve_server(false, None, None, Some("kordon")), None);
    }
}
