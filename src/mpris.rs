//! Thin MPRIS client over the session bus (blocking, libdbus via the `dbus` crate).
//!
//! Browsers (Firefox, Chromium, Chrome, Brave, Vivaldi, ...) and most media
//! apps register `org.mpris.MediaPlayer2.*` on the session bus. We talk to
//! that instead of anything browser-specific, so it keeps working when you
//! switch tabs, and nothing here needs polling: changes arrive as signals.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[allow(unused_imports)]
use dbus::arg::RefArg; // for `as_i64` on metadata values
use dbus::arg::{self, PropMap};
use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;
use dbus::blocking::{Connection, Proxy};
use dbus::message::MatchRule;
use dbus::strings::BusName;

use crate::model::{pick_player, Snapshot, Status, Track};

pub type Result<T> = std::result::Result<T, dbus::Error>;

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const OBJ: &str = "/org/mpris/MediaPlayer2";
const IFACE_ROOT: &str = "org.mpris.MediaPlayer2";
const IFACE_PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const CALL_TIMEOUT: Duration = Duration::from_millis(1500);

pub struct Client {
    conn: Connection,
    /// The player we showed last. Kept while it exists so the widget never
    /// jumps away or disappears just because it got paused.
    sticky: Option<String>,
    /// Last non-empty track per player. Browsers briefly report empty
    /// metadata while you switch tabs; we ride that out.
    last_good: HashMap<String, Track>,
    identities: HashMap<String, String>,
    changed: Arc<AtomicBool>,
}

impl Client {
    pub fn new() -> Result<Client> {
        Ok(Client {
            conn: Connection::new_session()?,
            sticky: None,
            last_good: HashMap::new(),
            identities: HashMap::new(),
            changed: Arc::new(AtomicBool::new(false)),
        })
    }

    fn proxy(&self, bus: &str) -> Result<Proxy<'static, &Connection>> {
        let name = BusName::new(bus.to_string()).map_err(|e| dbus::Error::new_failed(&e))?;
        Ok(self.conn.with_proxy(name, OBJ, CALL_TIMEOUT))
    }

    /// All MPRIS bus names currently on the bus, sorted.
    pub fn list_players(&self) -> Result<Vec<String>> {
        let p = self
            .conn
            .with_proxy("org.freedesktop.DBus", "/org/freedesktop/DBus", CALL_TIMEOUT);
        let (names,): (Vec<String>,) = p.method_call("org.freedesktop.DBus", "ListNames", ())?;
        let mut v: Vec<String> = names
            .into_iter()
            // playerctld just mirrors another player; skip the duplicate
            .filter(|n| n.starts_with(PREFIX) && !n.ends_with(".playerctld"))
            .collect();
        v.sort();
        Ok(v)
    }

    fn status_of(&self, bus: &str) -> Status {
        self.proxy(bus)
            .and_then(|p| p.get::<String>(IFACE_PLAYER, "PlaybackStatus"))
            .map(|s| Status::parse(&s))
            .unwrap_or(Status::Stopped)
    }

    fn identity_of(&mut self, bus: &str) -> String {
        if let Some(i) = self.identities.get(bus) {
            return i.clone();
        }
        let from_bus = || {
            let tail = bus.trim_start_matches(PREFIX);
            let base = tail.split('.').next().unwrap_or(tail);
            let mut c = base.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        };
        let id = self
            .proxy(bus)
            .and_then(|p| p.get::<String>(IFACE_ROOT, "Identity"))
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(from_bus);
        self.identities.insert(bus.to_string(), id.clone());
        id
    }

    /// Read everything we display from one player.
    pub fn snapshot(&mut self, bus: &str) -> Result<Snapshot> {
        let p = self.proxy(bus)?;
        let status = p
            .get::<String>(IFACE_PLAYER, "PlaybackStatus")
            .map(|s| Status::parse(&s))
            .unwrap_or(Status::Stopped);
        let volume = p.get::<f64>(IFACE_PLAYER, "Volume").unwrap_or(1.0);
        let position_us = p.get::<i64>(IFACE_PLAYER, "Position").unwrap_or(0);
        let can_seek = p.get::<bool>(IFACE_PLAYER, "CanSeek").unwrap_or(false);
        let meta = p.get::<PropMap>(IFACE_PLAYER, "Metadata").unwrap_or_default();

        let mut track = parse_track(&meta);
        if track.is_empty() && status != Status::Stopped {
            if let Some(t) = self.last_good.get(bus) {
                track = t.clone();
            }
        } else if !track.is_empty() {
            self.last_good.insert(bus.to_string(), track.clone());
        }

        let identity = self.identity_of(bus);
        Ok(Snapshot {
            bus_name: bus.to_string(),
            identity,
            status,
            track,
            volume,
            position_us,
            can_seek,
        })
    }

    /// Pick the player to show and read it. `Ok(None)` means no player exists.
    pub fn current(&mut self) -> Result<Option<Snapshot>> {
        let names = self.list_players()?;
        self.last_good.retain(|k, _| names.contains(k));
        self.identities.retain(|k, _| names.contains(k));

        let cands: Vec<(String, Status)> = names
            .iter()
            .map(|n| (n.clone(), self.status_of(n)))
            .collect();
        let idx = match pick_player(&cands, self.sticky.as_deref()) {
            Some(i) => i,
            None => {
                self.sticky = None;
                return Ok(None);
            }
        };
        let bus = names[idx].clone();
        self.sticky = Some(bus.clone());
        self.snapshot(&bus).map(Some)
    }

    // ---- commands -------------------------------------------------------

    fn call(&self, bus: &str, method: &str) -> Result<()> {
        let p = self.proxy(bus)?;
        let _: () = p.method_call(IFACE_PLAYER, method, ())?;
        Ok(())
    }

    pub fn play_pause(&self, bus: &str) -> Result<()> {
        self.call(bus, "PlayPause")
    }
    pub fn play(&self, bus: &str) -> Result<()> {
        self.call(bus, "Play")
    }
    pub fn pause(&self, bus: &str) -> Result<()> {
        self.call(bus, "Pause")
    }
    pub fn stop(&self, bus: &str) -> Result<()> {
        self.call(bus, "Stop")
    }
    pub fn next(&self, bus: &str) -> Result<()> {
        self.call(bus, "Next")
    }
    pub fn previous(&self, bus: &str) -> Result<()> {
        self.call(bus, "Previous")
    }

    pub fn set_volume(&self, bus: &str, volume: f64) -> Result<()> {
        let p = self.proxy(bus)?;
        p.set(IFACE_PLAYER, "Volume", volume.clamp(0.0, 1.0))
    }

    /// What the player says its volume is right now.
    pub fn volume(&self, bus: &str) -> Result<f64> {
        self.proxy(bus)?.get::<f64>(IFACE_PLAYER, "Volume")
    }

    /// Add `delta` (e.g. 0.05) to the player's volume.
    pub fn volume_step(&self, bus: &str, delta: f64) -> Result<f64> {
        let p = self.proxy(bus)?;
        let now = p.get::<f64>(IFACE_PLAYER, "Volume").unwrap_or(1.0);
        let new = (now + delta).clamp(0.0, 1.0);
        p.set(IFACE_PLAYER, "Volume", new)?;
        Ok(new)
    }

    /// Jump to an absolute position. Uses `SetPosition` when we know the track
    /// id and falls back to a relative `Seek`, which every player supports.
    pub fn seek_to(
        &self,
        bus: &str,
        track_id: Option<&str>,
        target_us: i64,
        current_us: i64,
    ) -> Result<()> {
        let p = self.proxy(bus)?;
        if let Some(id) = track_id {
            if let Ok(path) = dbus::Path::new(id.to_string()) {
                let r: Result<()> =
                    p.method_call(IFACE_PLAYER, "SetPosition", (path, target_us.max(0)));
                if r.is_ok() {
                    return Ok(());
                }
            }
        }
        let _: () = p.method_call(IFACE_PLAYER, "Seek", (target_us - current_us,))?;
        Ok(())
    }

    // ---- change notification ------------------------------------------

    /// Subscribe to everything that can change what we show. Call once.
    pub fn subscribe(&self) -> Result<()> {
        let flag = self.changed.clone();
        self.conn.add_match(
            MatchRule::new_signal("org.freedesktop.DBus.Properties", "PropertiesChanged")
                .with_path(OBJ),
            move |_: (), _, _| {
                flag.store(true, Ordering::SeqCst);
                true
            },
        )?;

        let flag = self.changed.clone();
        self.conn.add_match(
            MatchRule::new_signal(IFACE_PLAYER, "Seeked").with_path(OBJ),
            move |_: (), _, _| {
                flag.store(true, Ordering::SeqCst);
                true
            },
        )?;

        let flag = self.changed.clone();
        self.conn.add_match(
            MatchRule::new_signal("org.freedesktop.DBus", "NameOwnerChanged"),
            move |(name, _old, _new): (String, String, String), _, _| {
                if name.starts_with(PREFIX) {
                    flag.store(true, Ordering::SeqCst);
                }
                true
            },
        )?;
        Ok(())
    }

    /// Sleep (no CPU) until something relevant changes or `timeout` passes.
    /// Returns true if something changed. Bursts are coalesced into one.
    pub fn wait_for_change(&self, timeout: Duration) -> Result<bool> {
        self.conn.process(timeout)?;
        // a short quiet period merges the 3-5 signals a track change emits
        while self.conn.process(Duration::from_millis(40))? {}
        Ok(self.changed.swap(false, Ordering::SeqCst))
    }
}

fn parse_track(meta: &PropMap) -> Track {
    let string = |k: &str| arg::prop_cast::<String>(meta, k).cloned().unwrap_or_default();

    let artists = if let Some(v) = arg::prop_cast::<Vec<String>>(meta, "xesam:artist") {
        v.clone()
    } else if let Some(s) = arg::prop_cast::<String>(meta, "xesam:artist") {
        vec![s.clone()]
    } else {
        Vec::new()
    };

    // Length is `x` for well-behaved players; as_i64 also accepts u/t/i.
    let length_us = meta
        .get("mpris:length")
        .and_then(|v| v.0.as_i64())
        .unwrap_or(0);

    let track_id = arg::prop_cast::<dbus::Path<'static>>(meta, "mpris:trackid")
        .map(|p| p.to_string())
        .or_else(|| arg::prop_cast::<String>(meta, "mpris:trackid").cloned());

    let art = string("mpris:artUrl");

    Track {
        title: string("xesam:title"),
        artists,
        album: string("xesam:album"),
        art_url: if art.is_empty() { None } else { Some(art) },
        length_us,
        track_id,
    }
}
