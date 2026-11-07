//! `clearway-setup.exe`: installs or updates Clearway for the current user. Finds WSL distributions, Claude Code and
//! Codex, shows what it will do and why, asks once, then runs the embedded install scripts and the official plugin
//! commands. `--yes` skips the question, `--dry-run` stops after the plan.

#[cfg(windows)]
mod windows;

fn main() {
    #[cfg(windows)]
    std::process::exit(windows::main());
    #[cfg(not(windows))]
    {
        eprintln!("clearway-setup runs on Windows; in WSL use install.sh");
        std::process::exit(2);
    }
}
