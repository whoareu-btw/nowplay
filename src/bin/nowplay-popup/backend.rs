//! Two small worker threads keep D-Bus off the UI thread:
//!  - the watcher sleeps on D-Bus signals and pushes fresh snapshots,
//!  - the commander sends button/slider actions to the player.

use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use nowplay::model::Snapshot;
use nowplay::mpris::Client;

pub enum Event {
    State(Option<Snapshot>),
    /// Downloaded art for this URL is now available at this path.
    Art(String, PathBuf),
    /// The volume a player reports after we tried to change it. Some players
    /// ignore writes; this lets the slider show the truth instead of a guess.
    Volume(String, f64),
}

pub enum Cmd {
    /// Ask the player for its volume and report it back as `Event::Volume`.
    ReadVolume(String),
    PlayPause(String),
    Next(String),
    Prev(String),
    Volume(String, f64),
    Seek {
        bus: String,
        track_id: Option<String>,
        target_us: i64,
        current_us: i64,
    },
}

pub fn spawn_watcher(tx: async_channel::Sender<Event>) {
    thread::spawn(move || loop {
        let mut client = match Client::new().and_then(|c| c.subscribe().map(|_| c)) {
            Ok(c) => c,
            Err(_) => {
                thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        loop {
            let snap = match client.current() {
                Ok(s) => s,
                Err(_) => break, // bus hiccup: reconnect
            };
            if tx.send_blocking(Event::State(snap)).is_err() {
                return; // window is gone
            }
            if client.wait_for_change(Duration::from_secs(300)).is_err() {
                break;
            }
        }
        thread::sleep(Duration::from_secs(1));
    });
}

pub fn spawn_commander(ev_tx: async_channel::Sender<Event>) -> mpsc::Sender<Cmd> {
    let (tx, rx) = mpsc::channel::<Cmd>();
    thread::spawn(move || {
        let mut client: Option<Client> = None;
        while let Ok(first) = rx.recv() {
            // Dragging a slider floods us; only the newest volume/seek matters.
            let mut batch = vec![first];
            while let Ok(more) = rx.try_recv() {
                batch.push(more);
            }
            let last_vol = batch.iter().rposition(|c| matches!(c, Cmd::Volume(..)));
            let last_seek = batch.iter().rposition(|c| matches!(c, Cmd::Seek { .. }));

            for (i, cmd) in batch.into_iter().enumerate() {
                match &cmd {
                    Cmd::Volume(..) if Some(i) != last_vol => continue,
                    Cmd::Seek { .. } if Some(i) != last_seek => continue,
                    _ => {}
                }
                if client.is_none() {
                    client = Client::new().ok();
                }
                let c = match client.as_ref() {
                    Some(c) => c,
                    None => continue,
                };
                let result = match cmd {
                    Cmd::ReadVolume(b) => c.volume(&b).map(|v| {
                        let _ = ev_tx.send_blocking(Event::Volume(b, v));
                    }),
                    Cmd::PlayPause(b) => c.play_pause(&b),
                    Cmd::Next(b) => c.next(&b),
                    Cmd::Prev(b) => c.previous(&b),
                    Cmd::Volume(b, v) => c.set_volume(&b, v),
                    Cmd::Seek { bus, track_id, target_us, current_us } => {
                        c.seek_to(&bus, track_id.as_deref(), target_us, current_us)
                    }
                };
                if result.is_err() {
                    client = None; // reconnect on the next command
                }
            }
        }
    });
    tx
}
