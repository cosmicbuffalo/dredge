fn main() {
    if let Err(error) = dredge_cli::run() {
        eprintln!("dredge: {error:#}");
        std::process::exit(1);
    }
}
