fn main() {
    if let Some(endpoint) = askpass_endpoint() {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => std::process::exit(1),
        };
        let passphrase =
            match runtime.block_on(hermito::remote::ssh_identity::askpass_client(&endpoint)) {
                Ok(passphrase) => passphrase,
                Err(_) => std::process::exit(1),
            };
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        if stdout.write_all(&passphrase).is_err() || stdout.write_all(b"\n").is_err() {
            std::process::exit(1);
        }
        return;
    }

    // Thin entry. All terminal state, recovery, guard, signals, converge-restore live in lib.
    // Errors are printed only after the single consuming restore path has run.
    if let Err(err) = hermito::run() {
        eprintln!("hermito: {}", err);
        std::process::exit(1);
    }
}

fn askpass_endpoint() -> Option<String> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--ssh-askpass") => args.next(),
        _ => std::env::var("HERMITO_ASKPASS_ENDPOINT").ok(),
    }
}
