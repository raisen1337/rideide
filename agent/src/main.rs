//! The agent as a program of its own: what ride uploads to a remote host and runs there.

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("mcp") => return ride_agent::mcp::run(),
        #[cfg(unix)]
        Some("collab") => return ride_agent::collab::connect(),
        #[cfg(unix)]
        Some("collab-broker") => return ride_agent::collab::broker(),
        _ => {}
    }
    ride_agent::serve(std::io::stdin().lock(), std::io::stdout());
    // Threads serving terminals may still be blocked on their shells; the app is gone, so are we.
    std::process::exit(0);
}
