//! `nowplay`: waybar module + tiny remote control for MPRIS players.
//! No GUI, no async runtime, no polling: it sleeps until D-Bus wakes it.

use std::io::Write;
use std::process::ExitCode;
use std::thread::sleep;
use std::time::Duration;

use nowplay::model::{fmt_time, waybar_json, Snapshot, Status};
use nowplay::mpris::Client;

const HELP: &str = "\
nowplay - control browser/media players over MPRIS

USAGE:
    nowplay waybar [--max-len N] [--icon-only]
                                   stream waybar JSON, one line per change
                                   (default max-len 45; --icon-only shows just the play/pause icon)
    nowplay status                 print one line: state, artist - title, position, volume
    nowplay toggle | play | pause | stop | next | prev
    nowplay vol +5 | -5 | 40       change volume by percent, or set it
    nowplay help

The popup window is the separate `nowplay-popup` binary.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");

    match cmd {
        "waybar" => {
            let max_len = flag_value(&args, "--max-len")
                .and_then(|v| v.parse().ok())
                .unwrap_or(45);
            let icon_only = args.iter().any(|a| a == "--icon-only");
            waybar(max_len, icon_only)
        }
        "status" => with_current(|_, s| {
            println!("{}", status_line(&s));
            Ok(())
        }),
        "toggle" => with_current(|c, s| c.play_pause(&s.bus_name)),
        "play" => with_current(|c, s| c.play(&s.bus_name)),
        "pause" => with_current(|c, s| c.pause(&s.bus_name)),
        "stop" => with_current(|c, s| c.stop(&s.bus_name)),
        "next" => with_current(|c, s| c.next(&s.bus_name)),
        "prev" | "previous" => with_current(|c, s| c.previous(&s.bus_name)),
        "vol" | "volume" => {
            let arg = match args.get(1) {
                Some(a) => a.clone(),
                None => {
                    eprintln!("usage: nowplay vol +5 | -5 | 40");
                    return ExitCode::from(2);
                }
            };
            volume(&arg)
        }
        "help" | "-h" | "--help" => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("unknown command: {other}\n\n{HELP}");
            ExitCode::from(2)
        }
    }
}

fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let i = args.iter().position(|a| a == flag)?;
    args.get(i + 1).map(String::as_str)
}

/// Run `f` against the player that would be shown right now.
fn with_current<F>(f: F) -> ExitCode
where
    F: FnOnce(&Client, Snapshot) -> nowplay::mpris::Result<()>,
{
    let mut client = match Client::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("nowplay: cannot reach the session bus: {e}");
            return ExitCode::FAILURE;
        }
    };
    match client.current() {
        Ok(Some(snap)) => match f(&client, snap) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("nowplay: {e}");
                ExitCode::FAILURE
            }
        },
        Ok(None) => {
            eprintln!("nowplay: no player running");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("nowplay: {e}");
            ExitCode::FAILURE
        }
    }
}

fn volume(arg: &str) -> ExitCode {
    // "+5" / "-5" are steps, a bare number is an absolute percent
    let relative = arg.starts_with('+') || arg.starts_with('-');
    let pct: f64 = match arg.trim_start_matches('+').parse() {
        Ok(v) => v,
        Err(_) => {
            eprintln!("nowplay: not a number: {arg}");
            return ExitCode::from(2);
        }
    };
    with_current(|c, s| {
        let new = if relative {
            c.volume_step(&s.bus_name, pct / 100.0)?
        } else {
            let v = (pct / 100.0).clamp(0.0, 1.0);
            c.set_volume(&s.bus_name, v)?;
            v
        };
        println!("{}", (new * 100.0).round() as i64);
        Ok(())
    })
}

fn status_line(s: &Snapshot) -> String {
    let state = match s.status {
        Status::Playing => "playing",
        Status::Paused => "paused",
        Status::Stopped => "stopped",
    };
    let t = &s.track;
    let what = match (t.artists.is_empty(), t.title.is_empty()) {
        (false, false) => format!("{} - {}", t.artist_line(), t.title),
        (true, false) => t.title.clone(),
        (false, true) => t.artist_line(),
        (true, true) => s.identity.clone(),
    };
    format!(
        "[{state}] {what} ({} / {}) vol {}%",
        fmt_time(s.position_us),
        fmt_time(t.length_us),
        (s.volume.clamp(0.0, 1.0) * 100.0).round() as i64
    )
}

/// Long-running waybar feed. Survives the session bus not being up yet,
/// browsers starting and quitting, and the bus connection dropping.
fn waybar(max_len: usize, icon_only: bool) -> ExitCode {
    let stdout = std::io::stdout();
    let mut last = String::new();

    let mut emit = |line: String| -> bool {
        if line == last {
            return true;
        }
        let mut out = stdout.lock();
        let ok = writeln!(out, "{line}").is_ok() && out.flush().is_ok();
        last = line;
        ok // false = waybar closed the pipe, so we exit
    };

    loop {
        let mut client = match Client::new().and_then(|c| c.subscribe().map(|_| c)) {
            Ok(c) => c,
            Err(_) => {
                if !emit(waybar_json(None, max_len, icon_only)) {
                    return ExitCode::SUCCESS;
                }
                sleep(Duration::from_secs(2));
                continue;
            }
        };

        loop {
            let snap = match client.current() {
                Ok(s) => s,
                Err(_) => break, // reconnect
            };
            if !emit(waybar_json(snap.as_ref(), max_len, icon_only)) {
                return ExitCode::SUCCESS;
            }
            // Sleeps in the kernel until a signal arrives; the long timeout
            // only exists to resync once in a while.
            if client.wait_for_change(Duration::from_secs(300)).is_err() {
                break;
            }
        }
        sleep(Duration::from_secs(1));
    }
}
