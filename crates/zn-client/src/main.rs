mod app;

use app::ClientApp;

fn main() {
    env_logger::init();
    log::info!("ZONA client starting");

    let event_loop = winit::event_loop::EventLoop::new().expect("failed to create event loop");
    let mut client = ClientApp::new();
    if let Err(error) = event_loop.run_app(&mut client) {
        log::error!("client terminated with error: {error}");
        std::process::exit(1);
    }
}
