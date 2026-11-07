//! `clearway-setup.exe`: installs or updates Clearway for the current user. A window finds WSL, Claude Code and Codex,
//! shows what it will install and why, and installs what stays ticked. `--dry-run` prints the plan, `--yes` installs
//! it without the window (tests).
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod gui;
#[cfg(windows)]
mod windows;

fn main() {
    #[cfg(windows)]
    std::process::exit(windows::main());
    #[cfg(not(windows))]
    {
        eprintln!("clearway-setup runs on Windows");
        std::process::exit(2);
    }
}
