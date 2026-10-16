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
    level: String,
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
           --level <имя>          имя уровня из <content-root>/levels/<имя>.scene.ron\n                              \
                                   (по умолчанию {DEFAULT_LEVEL})\n  \
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
    let level = level.unwrap_or_else(|| DEFAULT_LEVEL.to_string());

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
        "ZONA client starting: content-root={}, level={}, server={}",
        cli.content_root.display(),
        cli.level,
        cli.server.as_deref().unwrap_or("<offline>")
    );

    let event_loop = winit::event_loop::EventLoop::new().expect("failed to create event loop");
    let mut client = match cli.server {
        Some(server) => ClientApp::with_server(server, cli.content_root, &cli.level),
        None => ClientApp::with_content(cli.content_root, &cli.level),
    };
    if let Err(error) = event_loop.run_app(&mut client) {
        log::error!("client terminated with error: {error}");
        std::process::exit(1);
    }
}
