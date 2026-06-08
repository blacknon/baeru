mod cli_backend;
mod config;
mod highlight;
mod keymap;
mod model;
mod support;
mod theme;
mod transform;
mod tui_backend;

use anyhow::{anyhow, Result};
use clap::Parser;
use model::{Backend, Cli, Runtime};
use std::{
    io::{self, Write},
    process,
};

fn main() {
    let exit_code = match real_main() {
        Ok(()) => 0,
        Err(err) => {
            support::restore_terminal_state();
            eprintln!("baeru: {err:#}");
            1
        }
    };

    process::exit(exit_code);
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let rt = config::build_runtime(cli)?;
    match rt.backend {
        Backend::Tui => tui_backend::run_tui_backend(rt),
        Backend::Cli => cli_backend::run_cli_backend(rt),
        Backend::Raw => run_raw_backend(rt),
        Backend::Auto => Err(anyhow!("unresolved backend")),
    }
}

fn run_raw_backend(rt: Runtime) -> Result<()> {
    if rt.command.is_empty() {
        let mut stdin = io::stdin().lock();
        let mut stdout = io::stdout().lock();
        io::copy(&mut stdin, &mut stdout)?;
        stdout.flush()?;
        return Ok(());
    }
    support::spawn_direct(&rt.command)
}
