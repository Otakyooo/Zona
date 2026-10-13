const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_CHANNEL: &str = "dev";

fn main() {
    let mut channel = DEFAULT_CHANNEL.to_string();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--channel" => {
                if let Some(value) = args.next() {
                    channel = value;
                }
            }
            "--version" => {
                println!("zn-launcher {VERSION}");
                return;
            }
            other => {
                eprintln!("unknown flag: {other}");
                std::process::exit(2);
            }
        }
    }

    log::info!("zn-launcher {VERSION} starting on channel '{channel}'");
    println!("ZONA launcher {VERSION} (channel: {channel})");
    println!("M0 stub — manifest-driven downloads from GitHub Releases arrive in M6.");
    println!("Run zn-client directly for the M1 vertical slice.");
}
