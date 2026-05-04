use anyhow::Result;
use crossterm::{cursor::Show, execute, terminal::disable_raw_mode};
use std::{
    ffi::OsString,
    io::{self, IsTerminal, Write},
    time::Duration,
};

pub(crate) fn restore_terminal_state() {
    if io::stdout().is_terminal() {
        let _ = execute!(io::stdout(), Show, crossterm::style::ResetColor);
        let _ = disable_raw_mode();
    }
}

pub(crate) fn exit_with_status(code: i32) -> ! {
    restore_terminal_state();
    std::process::exit(code);
}

pub(crate) fn spawn_direct(command: &[OsString]) -> Result<()> {
    let mut cmd = std::process::Command::new(&command[0]);
    cmd.args(&command[1..]);
    let status = cmd.status()?;
    exit_with_status(status.code().unwrap_or(1));
}

pub(crate) fn print_raw_text(text: &str) -> Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(text.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

pub(crate) fn sleep_frame(duration_ms: u64, frames: usize) {
    std::thread::sleep(Duration::from_millis(
        (duration_ms / frames.max(1) as u64).max(1),
    ));
}

pub(crate) fn env_flag(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

pub(crate) fn is_term_dumb() -> bool {
    std::env::var("TERM").map(|s| s == "dumb").unwrap_or(false)
}
