//! The `mudling` command line, for plugin authors (PRD §11):
//!
//! - `mudling plugin new <id>` scaffolds a plugin that passes its own test.
//! - `mudling plugin test` replays logs and shows what each pattern caught.
//!
//! Registry CI calls these, so their output and exit codes are a contract:
//! 0 passed, 1 the plugin failed, with a `fail <kind>` line for each reason,
//! and 2 the command itself couldn't run. docs/plugins/reference.md has the
//! details; a change here belongs there too.

mod check;
mod scaffold;

use check::Mode;
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage:
  mudling plugin new <id> [--dir <parent>]
  mudling plugin test [<folder>] [--log <file> | --update]";

const FAILED: u8 = 1;
const COULD_NOT_RUN: u8 = 2;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(FAILED),
        Err(problem) => {
            eprintln!("mudling: {problem}");
            ExitCode::from(COULD_NOT_RUN)
        }
    }
}

/// Whether the plugin passed; an error means the command couldn't run.
fn run(args: &[String]) -> Result<bool, String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["plugin", "new", rest @ ..] => new(rest),
        ["plugin", "test", rest @ ..] => test(rest),
        _ => Err(USAGE.to_owned()),
    }
}

fn new(mut args: &[&str]) -> Result<bool, String> {
    let (mut id, mut parent) = (None, PathBuf::from("."));
    while let Some((&arg, rest)) = args.split_first() {
        match (arg, rest) {
            ("--dir", [dir, rest @ ..]) => {
                parent = PathBuf::from(dir);
                args = rest;
            }
            (id_given, rest) if id.is_none() && !id_given.starts_with("--") => {
                id = Some(id_given);
                args = rest;
            }
            _ => return Err(USAGE.to_owned()),
        }
    }
    let id = id.ok_or(USAGE)?;
    let folder = scaffold::create(id, &parent)?;
    println!("created {}", folder.display());
    println!("next: mudling plugin test {}", folder.display());
    Ok(true)
}

fn test(mut args: &[&str]) -> Result<bool, String> {
    let (mut folder, mut log, mut update) = (None, None, false);
    while let Some((&arg, rest)) = args.split_first() {
        match (arg, rest) {
            ("--log", [file, rest @ ..]) if log.is_none() => {
                log = Some(PathBuf::from(file));
                args = rest;
            }
            ("--update", rest) => {
                update = true;
                args = rest;
            }
            (given, rest) if folder.is_none() && !given.starts_with("--") => {
                folder = Some(PathBuf::from(given));
                args = rest;
            }
            _ => return Err(USAGE.to_owned()),
        }
    }
    let mode = match (log, update) {
        (Some(_), true) => return Err(USAGE.to_owned()),
        (Some(log), false) => Mode::Log(log),
        (None, update) => Mode::OwnTests { update },
    };
    check::run(&folder.unwrap_or_else(|| PathBuf::from(".")), &mode)
}
