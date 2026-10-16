mod app;
mod gizmo;
mod orbit;
mod picking;

use app::EditorApp;
use std::path::PathBuf;

const DEFAULT_CONTENT_ROOT: &str = "game/content";

fn usage() -> String {
    "ZONA level editor\n\n\
     Использование:\n  \
       zn-editor [<level.scene.ron>] [--content-root <путь>]\n  \
       zn-editor --emit-demo <путь.scene.ron>\n\n\
     Опции:\n  \
       --content-root <путь>  корень контента для браузера ассетов (по умолчанию \
game/content)\n  \
       --emit-demo <путь>    записать demo_zone в файл и выйти\n  \
       -h, --help            эта справка\n"
        .into()
}

fn main() {
    env_logger::init();

    let mut level_path: Option<PathBuf> = None;
    let mut content_root: Option<PathBuf> = None;
    let mut emit_demo: Option<PathBuf> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--content-root" => match args.next() {
                Some(value) => content_root = Some(PathBuf::from(value)),
                None => {
                    eprintln!("--content-root требует значение\n{}", usage());
                    std::process::exit(2);
                }
            },
            "--emit-demo" => match args.next() {
                Some(value) => emit_demo = Some(PathBuf::from(value)),
                None => {
                    eprintln!("--emit-demo требует путь\n{}", usage());
                    std::process::exit(2);
                }
            },
            "-h" | "--help" => {
                print!("{}", usage());
                std::process::exit(0);
            }
            other if other.starts_with("--") => {
                eprintln!("неизвестная опция: {other}\n{}", usage());
                std::process::exit(2);
            }
            other => {
                if level_path.is_some() {
                    eprintln!("лишний аргумент: {other}\n{}", usage());
                    std::process::exit(2);
                }
                level_path = Some(PathBuf::from(other));
            }
        }
    }

    let content_root = content_root.unwrap_or_else(|| PathBuf::from(DEFAULT_CONTENT_ROOT));

    if let Some(path) = emit_demo {
        let scene = zn_scene::Scene::demo_zone();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        match scene.save_ron(&path) {
            Ok(()) => {
                println!(
                    "demo_zone записан в {} ({} объектов, spawn {:?})",
                    path.display(),
                    scene.len(),
                    scene.spawn
                );
                std::process::exit(0);
            }
            Err(error) => {
                eprintln!("ошибка записи {}: {error}", path.display());
                std::process::exit(1);
            }
        }
    }

    let (scene, current_path) = match level_path {
        Some(path) => match zn_scene::Scene::load_ron(&path) {
            Ok(scene) => {
                log::info!("уровень открыт из {}", path.display());
                (scene, Some(path))
            }
            Err(error) => {
                log::warn!("не удалось открыть {}: {error}", path.display());
                log::warn!("стартуем с demo_zone — сохраните файл, чтобы зафиксировать");
                (zn_scene::Scene::demo_zone(), Some(path))
            }
        },
        None => (zn_scene::Scene::demo_zone(), None),
    };

    let event_loop = winit::event_loop::EventLoop::new().expect("failed to create event loop");
    let mut editor = EditorApp::new(scene, current_path, content_root);
    if let Err(error) = event_loop.run_app(&mut editor) {
        log::error!("editor terminated with error: {error}");
        std::process::exit(1);
    }
}
