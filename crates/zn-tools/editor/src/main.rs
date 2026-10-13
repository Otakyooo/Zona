use zn_scene::Scene;

fn main() {
    env_logger::init();
    println!("ZONA level editor (M0 stub) — egui-based editor arrives in M2.");

    let scene = Scene::demo_zone();
    println!(
        "scene '{}': {} objects ({} solid), spawn {:?}",
        scene.name,
        scene.len(),
        scene.solids().count(),
        scene.spawn
    );
}
