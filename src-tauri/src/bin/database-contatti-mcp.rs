fn main() {
    if let Err(error) = database_contatti_lib::run_mcp() {
        eprintln!("[mcp] {error}");
        std::process::exit(1);
    }
}
