// huddle: a personal scrum board for Claude Code. One binary: commands print JSON for Claude, no command opens the board.
mod board;
mod cli;
mod time;
mod tui;
mod ui;

use std::io::IsTerminal;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = if args.is_empty() && std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        tui::main().map(|_| None)
    } else {
        board::open_db(&board::db_file()).and_then(|db| cli::run(&args, &db)).map(Some)
    };
    match out {
        Ok(None) => {}
        Ok(Some(cli::Out::Text(t))) => println!("{t}"),
        Ok(Some(cli::Out::Json(v))) => println!("{v}"),
        Err(e) => {
            eprintln!("Error: {e}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}
