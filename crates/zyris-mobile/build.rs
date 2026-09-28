// The Kotlin commands `src/lib.rs` calls. None is reachable from the window: Rust calls them.
const COMMANDS: &[&str] = &[];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).android_path("android").build();
}
