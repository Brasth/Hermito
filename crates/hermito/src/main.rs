fn main() {
    // Thin entry. All terminal state, recovery, guard, signals, converge-restore live in lib.
    // Errors are printed only after the single consuming restore path has run.
    if let Err(err) = hermito::run() {
        eprintln!("hermito: {}", err);
        std::process::exit(1);
    }
}
