#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
compile_error!("Database Contatti supports only Windows and macOS");

fn main() {
    database_contatti_lib::run()
}
