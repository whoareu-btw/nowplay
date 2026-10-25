//! The popup window. Everything here runs on the GTK main thread.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gtk4 as gtk;
use gtk::prelude::*;
use gtk::{gdk, glib};

use nowplay::art;
use nowplay::model::*;

use crate::backend::{Cmd, Event};

const CSS: &str = include_str!("style.css");
/// After the user touches a slider, ignore incoming values for this long
/// so the thumb does not fight their finger.
const USER_HOLD: Duration = Duration::from_millis(700);

struct State {
    snap: Option<Snapshot>,
    clock: PositionClock,
    shown_art: Option<String>,
    user_seek_at: Option<Instant>,
    user_vol_at: Option<Instant>,
}

fn recent(t: Option<Instant>) -> bool {
    t.map_or(false, |t| t.elapsed() < USER_HOLD)
}

pub struct Ui {
    pub win: gtk::ApplicationWindow,
    stack: gtk::Stack,
    picture: gtk::Picture,
    title: gtk::Label,
    artist: gtk::Label,
    album: gtk::Label,
    source: gtk::Label,
    pos: gtk::Label,
    dur: gtk::Label,
    progress: gtk::Scale,
    prev: gtk::Button,
    play: gtk::Button,
    next: gtk::Button,
    vol_icon: gtk::Label,
    vol: gtk::Scale,
    vol_pct: gtk::Label,
    st: RefCell<State>,
    cmd: mpsc::Sender<Cmd>,
    ev_tx: async_channel::Sender<Event>,
    pending_vol: Cell<Option<f64>>,
    /// Bumped on every volume touch; only the newest touch schedules a read-back.
    vol_gen: Cell<u32>,
    /// (target position, position when the drag started)
    pending_seek: Cell<Option<(i64, i64)>>,
}

fn text_label(css: &str) -> gtk::Label {
    let l = gtk::Label::new(None);
    l.set_xalign(0.0);
    l.set_ellipsize(gtk::pango::EllipsizeMode::End);
    l.set_max_width_chars(26);
    l.set_hexpand(true);
    l.add_css_class(css);
    l
}

/// Set one text row. An empty value becomes a zero-width space so the row
/// keeps its height instead of collapsing and pulling the others up.
fn set_line(label: &gtk::Label, text: &str) {
    label.set_text(if text.is_empty() { "\u{200b}" } else { text });
}

fn icon_label(ch: char, extra: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(&ch.to_string()));
    l.add_css_class("icon");
    if !extra.is_empty() {
        l.add_css_class(extra);
    }
    l
}

fn icon_button(ch: char, extra: &str) -> gtk::Button {
    let b = gtk::Button::with_label(&ch.to_string());
    b.add_css_class("icon");
    b.add_css_class("ctl");
    if !extra.is_empty() {
        b.add_css_class(extra);
    }
    b.set_focus_on_click(false);
    b
}

pub fn install_css() {
    if let Some(s) = gtk::Settings::default() {
        s.set_gtk_application_prefer_dark_theme(true);
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_data(CSS);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

impl Ui {
    pub fn new(
        app: &gtk::Application,
        cmd: mpsc::Sender<Cmd>,
        ev_tx: async_channel::Sender<Event>,
    ) -> Rc<Ui> {
        let win = gtk::ApplicationWindow::builder()
            .application(app)
            .title("nowplay")
            .decorated(false)
            .resizable(false)
            .default_width(380)
            .build();
        win.set_size_request(380, -1); // fixed width, whatever the title length
        win.add_css_class("np");

        // --- art (with a note-icon placeholder) -------------------------
        let picture = gtk::Picture::new();
        let ph = icon_label(ICON_MUSIC, "ph");
        let stack = gtk::Stack::new();
        stack.add_named(&picture, Some("art"));
        stack.add_named(&ph, Some("ph"));
        stack.set_visible_child_name("ph");
        stack.set_size_request(112, 112);
        stack.set_valign(gtk::Align::Start);
        stack.set_overflow(gtk::Overflow::Hidden);
        stack.add_css_class("art");

        // --- text -----------------------------------------------------
        let title = text_label("title");
        let artist = text_label("artist");
        let album = text_label("dim");
        let source = text_label("dim");
        let info = gtk::Box::new(gtk::Orientation::Vertical, 3);
        info.set_hexpand(true);
        info.set_valign(gtk::Align::Center);
        // Same height as the art, and all four rows always exist (see
        // `set_line`), so the block never grows, shrinks or shifts.
        info.set_size_request(-1, 112);
        for l in [&title, &artist, &album, &source] {
            info.append(l);
        }

        let top = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        top.append(&stack);
        top.append(&info);

        // --- progress -------------------------------------------------
        // fixed-width time labels so the seek bar does not change length
        let pos = gtk::Label::new(Some("0:00"));
        pos.add_css_class("time");
        pos.set_width_chars(5);
        pos.set_xalign(0.0);
        let dur = gtk::Label::new(Some("0:00"));
        dur.add_css_class("time");
        dur.set_width_chars(5);
        dur.set_xalign(1.0);
        let progress = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.001);
        progress.set_draw_value(false);
        progress.set_hexpand(true);
        let prog_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        prog_row.append(&pos);
        prog_row.append(&progress);
        prog_row.append(&dur);

        // --- transport ------------------------------------------------
        let prev = icon_button(ICON_PREV, "");
        let play = icon_button(ICON_PLAY, "play");
        let next = icon_button(ICON_NEXT, "");
        let ctl_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        ctl_row.set_halign(gtk::Align::Center);
        ctl_row.append(&prev);
        ctl_row.append(&play);
        ctl_row.append(&next);

        // --- volume ---------------------------------------------------
        let vol_icon = icon_label(ICON_VOL_HIGH, "");
        vol_icon.set_width_chars(2);
        let vol = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
        vol.set_draw_value(false);
        vol.set_hexpand(true);
        let vol_pct = gtk::Label::new(Some("100%"));
        vol_pct.add_css_class("time");
        vol_pct.set_width_chars(4);
        vol_pct.set_xalign(1.0);
        let vol_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        vol_row.append(&vol_icon);
        vol_row.append(&vol);
        vol_row.append(&vol_pct);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
        root.add_css_class("card");
        root.append(&top);
        root.append(&prog_row);
        root.append(&ctl_row);
        root.append(&vol_row);
        win.set_child(Some(&root));

        let ui = Rc::new(Ui {
            win,
            stack,
            picture,
            title,
            artist,
            album,
            source,
            pos,
            dur,
            progress,
            prev,
            play,
            next,
            vol_icon,
            vol,
            vol_pct,
            st: RefCell::new(State {
                snap: None,
                clock: PositionClock::new(0, false),
                shown_art: None,
                user_seek_at: None,
                user_vol_at: None,
            }),
            cmd,
            ev_tx,
            pending_vol: Cell::new(None),
            vol_gen: Cell::new(0),
            pending_seek: Cell::new(None),
        });
        ui.connect_signals();
        ui.apply(None);
        ui
    }

    fn connect_signals(self: &Rc<Self>) {
        let ui = self.clone();
        self.play.connect_clicked(move |_| {
            if let Some(b) = ui.bus() {
                let _ = ui.cmd.send(Cmd::PlayPause(b));
            }
        });
        let ui = self.clone();
        self.prev.connect_clicked(move |_| {
            if let Some(b) = ui.bus() {
                let _ = ui.cmd.send(Cmd::Prev(b));
            }
        });
        let ui = self.clone();
        self.next.connect_clicked(move |_| {
            if let Some(b) = ui.bus() {
                let _ = ui.cmd.send(Cmd::Next(b));
            }
        });

        // `change-value` fires only for user input, never for set_value(),
        // so our own updates cannot loop back into the player.
        let ui = self.clone();
        self.vol.connect_change_value(move |_, _, v| {
            on_volume_user(&ui, v.clamp(0.0, 100.0));
            glib::Propagation::Proceed
        });
        let ui = self.clone();
        self.progress.connect_change_value(move |_, _, v| {
            on_seek_user(&ui, v);
            glib::Propagation::Proceed
        });
    }

    fn bus(&self) -> Option<String> {
        self.st.borrow().snap.as_ref().map(|s| s.bus_name.clone())
    }

    pub fn handle(self: &Rc<Self>, ev: Event) {
        match ev {
            Event::State(snap) => self.apply(snap),
            Event::Art(url, path) => {
                let wanted = self.st.borrow().shown_art.as_deref() == Some(url.as_str());
                if wanted {
                    self.set_art_file(&path);
                }
            }
            Event::Volume(bus, v) => {
                let (same, held) = {
                    let st = self.st.borrow();
                    (
                        matches!(&st.snap, Some(s) if s.bus_name == bus),
                        recent(st.user_vol_at),
                    )
                };
                if same && !held {
                    if let Some(s) = self.st.borrow_mut().snap.as_mut() {
                        s.volume = v;
                    }
                    self.vol.set_value(v.clamp(0.0, 1.0) * 100.0);
                    self.set_volume_widgets(v);
                }
            }
        }
    }

    /// Show a fresh snapshot (or the empty state when `None`).
    fn apply(self: &Rc<Self>, snap: Option<Snapshot>) {
        let s = match snap {
            Some(s) => s,
            None => {
                self.st.borrow_mut().snap = None;
                set_line(&self.title, "Nothing playing");
                self.title.set_tooltip_text(None);
                set_line(&self.artist, "Start a song or video in your browser");
                set_line(&self.album, "");
                set_line(&self.source, "");
                self.show_art(None);
                self.play.set_label(&ICON_PLAY.to_string());
                for b in [&self.prev, &self.play, &self.next] {
                    b.set_sensitive(false);
                }
                self.progress.set_sensitive(false);
                self.progress.set_value(0.0);
                self.vol.set_sensitive(false);
                self.pos.set_text("0:00");
                self.dur.set_text("0:00");
                return;
            }
        };

        let playing = s.status == Status::Playing;
        {
            let mut st = self.st.borrow_mut();
            // A snapshot taken right after the user seeks can still hold the
            // old position; keep our optimistic clock for a moment.
            if !recent(st.user_seek_at) {
                st.clock = PositionClock::new(s.position_us, playing);
            } else {
                let at = st.clock.now_us(s.track.length_us);
                st.clock = PositionClock::new(at, playing);
            }
        }

        let t = &s.track;
        let title: &str = if t.title.is_empty() { &s.identity } else { &t.title };
        set_line(&self.title, title);
        self.title.set_tooltip_text(Some(title));
        set_line(&self.artist, &t.artist_line());
        set_line(&self.album, &t.album);
        set_line(&self.source, &s.identity);
        self.show_art(t.art_url.as_deref());

        self.play.set_label(&(if playing { ICON_PAUSE } else { ICON_PLAY }).to_string());
        for b in [&self.prev, &self.play, &self.next] {
            b.set_sensitive(true);
        }
        self.progress.set_sensitive(s.can_seek && t.length_us > 0);
        self.vol.set_sensitive(true);

        let hold_vol = recent(self.st.borrow().user_vol_at);
        if !hold_vol {
            self.vol.set_value(s.volume.clamp(0.0, 1.0) * 100.0);
            self.set_volume_widgets(s.volume);
        }

        self.dur.set_text(&if t.length_us > 0 { fmt_time(t.length_us) } else { "--:--".into() });
        self.st.borrow_mut().snap = Some(s);
        self.refresh_time();
    }

    fn set_volume_widgets(&self, v: f64) {
        let v = v.clamp(0.0, 1.0);
        self.vol_icon.set_text(&volume_icon(v).to_string());
        self.vol_pct.set_text(&format!("{}%", (v * 100.0).round() as i64));
    }

    /// Once a second while playing. Pure local arithmetic: no D-Bus traffic.
    pub fn tick(&self) {
        let playing = matches!(&self.st.borrow().snap, Some(s) if s.status == Status::Playing);
        if playing {
            self.refresh_time();
        }
    }

    fn refresh_time(&self) {
        let st = self.st.borrow();
        let snap = match &st.snap {
            Some(s) => s,
            None => return,
        };
        let len = snap.track.length_us;
        let pos = st.clock.now_us(len);

        let text = fmt_time(pos);
        if self.pos.text().as_str() != text {
            self.pos.set_text(&text);
        }
        if len > 0 && !recent(st.user_seek_at) {
            self.progress.set_value((pos as f64 / len as f64).clamp(0.0, 1.0));
        }
    }

    // ---- album art ------------------------------------------------------

    fn show_art(self: &Rc<Self>, url: Option<&str>) {
        {
            let mut st = self.st.borrow_mut();
            if st.shown_art.as_deref() == url {
                return;
            }
            st.shown_art = url.map(str::to_string);
        }
        // Until the right picture is ready, show the placeholder rather than
        // the previous track's art.
        self.stack.set_visible_child_name("ph");
        let url = match url {
            Some(u) => u,
            None => return,
        };
        if let Some(p) = art::local_path(url) {
            self.set_art_file(&p);
        } else if url.starts_with("http://") || url.starts_with("https://") {
            let tx = self.ev_tx.clone();
            let url = url.to_string();
            std::thread::spawn(move || {
                if let Some(p) = art::fetch(&url) {
                    let _ = tx.send_blocking(Event::Art(url, p));
                }
            });
        }
    }

    fn set_art_file(&self, path: &Path) {
        match gdk::Texture::from_filename(path) {
            Ok(tex) => {
                self.picture.set_paintable(Some(&tex));
                self.stack.set_visible_child_name("art");
            }
            Err(_) => self.stack.set_visible_child_name("ph"),
        }
    }
}

// Debounced user input: sliders emit dozens of events per second; we send the
// newest value at most every few dozen milliseconds.

fn on_volume_user(ui: &Rc<Ui>, v: f64) {
    ui.st.borrow_mut().user_vol_at = Some(Instant::now());
    ui.set_volume_widgets(v / 100.0);

    // Once the user has been quiet for longer than USER_HOLD, ask the player
    // what its volume really is, so a player that ignores writes shows the truth.
    let generation = ui.vol_gen.get().wrapping_add(1);
    ui.vol_gen.set(generation);
    {
        let ui = ui.clone();
        glib::timeout_add_local_once(USER_HOLD + Duration::from_millis(250), move || {
            if ui.vol_gen.get() == generation {
                if let Some(bus) = ui.bus() {
                    let _ = ui.cmd.send(Cmd::ReadVolume(bus));
                }
            }
        });
    }

    let armed = ui.pending_vol.get().is_some();
    ui.pending_vol.set(Some(v));
    if !armed {
        let ui = ui.clone();
        glib::timeout_add_local_once(Duration::from_millis(60), move || {
            if let (Some(v), Some(bus)) = (ui.pending_vol.take(), ui.bus()) {
                let _ = ui.cmd.send(Cmd::Volume(bus, (v / 100.0).clamp(0.0, 1.0)));
            }
        });
    }
}

fn on_seek_user(ui: &Rc<Ui>, frac: f64) {
    let (len, before, playing) = {
        let st = ui.st.borrow();
        match &st.snap {
            Some(s) => (
                s.track.length_us,
                st.clock.now_us(s.track.length_us),
                s.status == Status::Playing,
            ),
            None => return,
        }
    };
    if len <= 0 {
        return;
    }
    let target = (frac.clamp(0.0, 1.0) * len as f64) as i64;
    {
        // show the new time immediately; the player confirms shortly after
        let mut st = ui.st.borrow_mut();
        st.user_seek_at = Some(Instant::now());
        st.clock = PositionClock::new(target, playing);
    }
    ui.refresh_time();

    let armed = ui.pending_seek.get();
    let origin = armed.map(|(_, o)| o).unwrap_or(before);
    ui.pending_seek.set(Some((target, origin)));
    if armed.is_none() {
        let ui = ui.clone();
        glib::timeout_add_local_once(Duration::from_millis(120), move || {
            if let Some((target_us, current_us)) = ui.pending_seek.take() {
                let (bus, track_id) = {
                    let st = ui.st.borrow();
                    match &st.snap {
                        Some(s) => (s.bus_name.clone(), s.track.track_id.clone()),
                        None => return,
                    }
                };
                let _ = ui.cmd.send(Cmd::Seek { bus, track_id, target_us, current_us });
            }
        });
    }
}
