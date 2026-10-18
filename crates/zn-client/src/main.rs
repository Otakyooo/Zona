mod app;
mod assets;
mod interp;
mod level;
mod net;
mod prediction;

use app::ClientApp;
use level::{DEFAULT_CONTENT_ROOT, DEFAULT_LEVEL};

use std::path::PathBuf;

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
           zn-client [--content-root <путь>] [--level <имя>] [--server host:port]\n\n\
         Опции:\n  \
           --content-root <путь>  корень контента (по умолчанию {DEFAULT_CONTENT_ROOT});\n                              \
                                   переменная окружения ZN_CONTENT_ROOT переопределяет\n  \
           --level <имя>          одиночная сцена из <content-root>/levels/<имя>.scene.ron:\n                              \
                                   offline — по умолчанию {DEFAULT_LEVEL};\n                              \
                                   с сервером без опции загружается мир локаций (locations.ron)\n  \
           --server host:port     подключиться к UDP-серверу (без опции — offline)\n  \
           -h, --help             эта справка\n"
    )
}

fn parse_args() -> Cli {
    let mut content_root: Option<PathBuf> = None;
    let mut level: Option<String> = None;
    let mut server: Option<String> = None;
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

    Cli {
        content_root,
        level,
        server,
    }
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
