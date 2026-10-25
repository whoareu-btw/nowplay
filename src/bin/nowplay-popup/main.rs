//! `nowplay-popup`: the small dark window. Run it again (e.g. from a waybar
//! click) while it is open and it closes, so one module button toggles it.

mod backend;
mod ui;

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4 as gtk;
use gtk::glib;
use gtk::prelude::*;

const HELP: &str = "\
nowplay-popup - album art, track info, seek and volume for your browser's music

USAGE:
    nowplay-popup [--no-autoclose] [--anchor POS] [--margin-x PX] [--margin-y PX]

    --no-autoclose   keep the window open when it loses focus
    --anchor POS     (layer-shell build) top | top-right | top-left | bottom | bottom-right | bottom-left
                     Default `top`: centred, right below a top bar (a bottom anchor sits
                     above a bottom bar), because the compositor keeps it out of the
                     space the bar reserves.
    --margin-y PX    (layer-shell build) gap between the bar and the popup, default 8
    --margin-x PX    (layer-shell build) distance from the left or right screen edge
                     (not used with plain `top` / `bottom`, which are centred), default 8
    --margin PX      shorthand that sets both of the above

Keys: Esc closes. Launch it again while it is open to close it (toggle).
";

#[derive(Clone)]
struct Opts {
    autoclose: bool,
    #[allow(dead_code)]
    anchor: String,
    #[allow(dead_code)]
    margin_x: i32,
    #[allow(dead_code)]
    margin_y: i32,
}

impl Opts {
    fn parse(args: &[String]) -> Opts {
        let value = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .and_then(|i| args.get(i + 1))
                .cloned()
        };
        let num = |flag: &str| value(flag).and_then(|m| m.parse::<i32>().ok());
        let both = num("--margin").unwrap_or(8);
        Opts {
            autoclose: !args.iter().any(|a| a == "--no-autoclose"),
            anchor: value("--anchor").unwrap_or_else(|| "top".into()),
            margin_x: num("--margin-x").unwrap_or(both).max(0),
            margin_y: num("--margin-y").unwrap_or(both).max(0),
        }
    }
}

/// While a new popup is still mapping, compositors can hand focus to it and
/// take it away again within a few milliseconds. That is not the user
/// clicking elsewhere, so for this long after opening we never close the
/// popup because of focus, and never treat a second launch as "toggle off"
/// (a click that spawns the command twice would otherwise close it at once).
const OPEN_GRACE: Duration = Duration::from_millis(700);

/// When the popup closes itself because focus moved (usually onto the bar
/// you are about to click), remember it. The click then launches us again;
/// without this the click would close and instantly reopen the popup.
fn stamp_path() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("nowplay-popup.closed"))
}

fn mark_autoclosed() {
    if let Some(p) = stamp_path() {
        let _ = std::fs::write(p, b"");
    }
}

fn recently_autoclosed() -> bool {
    stamp_path()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
        .map_or(false, |age| age < Duration::from_millis(400))
}

#[cfg(feature = "layer-shell")]
fn setup_layer(win: &gtk::ApplicationWindow, o: &Opts) {
    use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
    use nowplay::model::parse_anchor;
    if !gtk4_layer_shell::is_supported() {
        return; // X11 or a compositor without layer-shell: stay a normal window
    }
    win.init_layer_shell();
    win.set_layer(Layer::Overlay);
    win.set_keyboard_mode(KeyboardMode::OnDemand);
    let (top, bottom, left, right) = parse_anchor(&o.anchor);
    win.set_anchor(Edge::Top, top);
    win.set_anchor(Edge::Bottom, bottom);
    win.set_anchor(Edge::Left, left);
    win.set_anchor(Edge::Right, right);
    // exclusive zone stays 0: the compositor then places us outside the
    // space the bar reserved, i.e. directly under (or over) it.
    win.set_margin(Edge::Top, if top { o.margin_y } else { 0 });
    win.set_margin(Edge::Bottom, if bottom { o.margin_y } else { 0 });
    win.set_margin(Edge::Left, if left { o.margin_x } else { 0 });
    win.set_margin(Edge::Right, if right { o.margin_x } else { 0 });
}

#[cfg(not(feature = "layer-shell"))]
fn setup_layer(_win: &gtk::ApplicationWindow, _o: &Opts) {}

fn build(app: &gtk::Application, opts: &Opts, opened: &Rc<Cell<Option<Instant>>>) {
    let shown_at = Instant::now();
    opened.set(Some(shown_at));
    ui::install_css();

    let (ev_tx, ev_rx) = async_channel::unbounded::<backend::Event>();
    let cmd_tx = backend::spawn_commander(ev_tx.clone());
    let ui = ui::Ui::new(app, cmd_tx, ev_tx.clone());
    let win = ui.win.clone();

    // D-Bus snapshots and downloaded art arrive here, on the main thread
    {
        let ui = ui.clone();
        glib::spawn_future_local(async move {
            while let Ok(ev) = ev_rx.recv().await {
                ui.handle(ev);
            }
        });
    }
    backend::spawn_watcher(ev_tx);

    // 1 Hz, only does arithmetic, only while a track is playing
    {
        let ui = ui.clone();
        glib::timeout_add_seconds_local(1, move || {
            ui.tick();
            glib::ControlFlow::Continue
        });
    }

    // Esc closes
    {
        let key = gtk::EventControllerKey::new();
        let w = win.clone();
        key.connect_key_pressed(move |_, k, _, _| {
            if k == gtk::gdk::Key::Escape {
                w.close();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        win.add_controller(key);
    }

    // Close when focus moves away (only after we really had focus, and not
    // for focus changes that happen while the window is still opening)
    if opts.autoclose {
        let had_focus = Rc::new(Cell::new(false));
        win.connect_is_active_notify(move |w| {
            if w.is_active() {
                had_focus.set(true);
            } else if had_focus.get() && shown_at.elapsed() >= OPEN_GRACE {
                mark_autoclosed();
                w.close();
            }
        });
    }

    setup_layer(&win, opts);
    win.present();
}

fn main() -> glib::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return glib::ExitCode::SUCCESS;
    }
    if recently_autoclosed() {
        return glib::ExitCode::SUCCESS;
    }
    // GTK4 would otherwise bring up the GPU stack for a 380px window.
    if std::env::var_os("GSK_RENDERER").is_none() {
        std::env::set_var("GSK_RENDERER", "cairo");
    }

    let opts = Opts::parse(&args);
    let app = gtk::Application::builder()
        .application_id("dev.nowplay.popup")
        .build();
    let opened: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
    app.connect_activate(move |app| {
        // second launch while open = toggle off (but not within the opening grace)
        if let Some(w) = app.active_window() {
            let too_soon = opened.get().map_or(false, |t| t.elapsed() < OPEN_GRACE);
            if !too_soon {
                w.close();
            }
            return;
        }
        build(app, &opts, &opened);
    });
    // our own flags are handled above; keep GApplication from parsing them
    app.run_with_args(&["nowplay-popup"])
}
