#[cfg(unix)]
mod client;

#[cfg(unix)]
fn main() {
    client::main();
}

#[cfg(not(unix))]
fn main() {
    eprintln!("charge : client prévu pour WSL (Linux).");
    std::process::exit(2);
}
