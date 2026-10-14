//! The agent as a program of its own: what ride uploads to a remote host and runs there.

fn main() {
    if std::env::args().nth(1).as_deref() == Some("mcp") {
        return ride_agent::mcp::run();
    }
    ride_agent::serve(std::io::stdin().lock(), std::io::stdout());
    // Threads serving terminals may still be blocked on their shells; the app is gone, so are we.
    std::process::exit(0);
}
