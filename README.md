# nowplay

A small music controller for Firefox, Chromium, Chrome, Brave, Vivaldi and anything else that speaks MPRIS. Two binaries:

- **`nowplay`** has no GUI. It is the waybar module and a remote (`toggle`, `next`, `vol +5`, ...). It only needs libdbus.
- **`nowplay-popup`** is a small dark GTK4 window with album art, title / artist / album, a seek bar with `m:ss` times, transport buttons and a volume slider. Nerd Font icons.

> **Status: not compiled yet.** I wrote this in a sandbox that could not reach crates.io or install GTK/libdbus headers, so the full build has never run, and neither binary has been tried against a real browser. What was checked: 17 unit tests on the pure logic (player selection, time format, waybar JSON, URI decoding, position clock), a type-check of the core and CLI against a stub of the `dbus` crate API, and a syntax parse of every file. The GTK popup has not been type-checked at all. Expect a compile error or two; send them back and they are quick to fix.

## Why it should not vanish when you switch tabs

- It talks to the browser's MPRIS service, not to a tab. The service stays up while the browser is open.
- It remembers the player it last showed and keeps showing it when paused, instead of jumping to another player or hiding.
- Browsers briefly report empty metadata during a tab switch. The last good title / artist / art is kept until real metadata comes back.
- The waybar module is cleared only when no MPRIS player exists at all (for example you closed the browser). Waybar hides an empty module.

## Why it is light

- No polling and no async runtime. `nowplay waybar` sleeps in the kernel until D-Bus sends a signal. Bursts (a track change sends several signals) are merged into one update. A 5-minute timer only resyncs.
- The popup's 1 Hz timer does arithmetic only (position advances locally, no D-Bus calls) and only while something is playing.
- Slider drags are debounced and coalesced, so a drag sends a few D-Bus calls, not hundreds.
- The popup uses GTK's cairo renderer (set automatically; override with `GSK_RENDERER`) so it does not start the GPU stack for a 380 px window.
- Release profile is `opt-level = "s"`, LTO, `panic = "abort"`, stripped.
- `./install.sh --cli-only` builds only the waybar module / remote, with `dbus` as its single dependency.
- Album art is cached in `~/.cache/nowplay` (at most about 200 files).

## Install

Dependencies:

| Distro | Packages |
|---|---|
| Arch | `rust gtk4 dbus pkgconf` (+ `gtk4-layer-shell` for `--layer-shell`) |
| Debian / Ubuntu | `cargo libgtk-4-dev libdbus-1-dev pkg-config` (+ `libgtk4-layer-shell-dev` for `--layer-shell`) |
| Fedora | `cargo gtk4-devel dbus-devel pkgconf` (+ `gtk4-layer-shell-devel`) |

You also need a **Nerd Font** (or Symbols Nerd Font as a fallback) for the icons. GTK 4.6 or newer is required.

```sh
./install.sh                 # nowplay + nowplay-popup into ~/.local/bin
./install.sh --layer-shell   # popup anchored top-right under your bar (Wayland)
./install.sh --cli-only      # waybar module only, no GTK needed
```

Make sure `~/.local/bin` is on the `PATH` that waybar starts with.

## NixOS

```sh
git init && git add -A                    # flakes only see files git knows about
nix develop -c cargo generate-lockfile    # one time: creates Cargo.lock (Nix needs it)
git add Cargo.lock
nix build                                 # ./result/bin/nowplay and ./result/bin/nowplay-popup
```

Outputs: `packages.<system>.default` (waybar module + popup), `nowplay-layer-shell` (popup anchors itself under the bar), `nowplay-cli` (waybar module only, no GTK), plus `overlays.default` and a `devShells.default` with everything needed for `cargo build`.

In your config, with this flake as an input named `nowplay`:

```nix
environment.systemPackages = [ inputs.nowplay.packages.${pkgs.stdenv.hostPlatform.system}.default ];
fonts.packages = [ pkgs.nerd-fonts.symbols-only ];
```

For waybar through home-manager, use store paths so `PATH` does not matter:

```nix
programs.waybar.settings.mainBar."custom/music" =
  let np = inputs.nowplay.packages.${pkgs.stdenv.hostPlatform.system}.default; in {
    exec = "${np}/bin/nowplay waybar --max-len 45";
    return-type = "json";
    format = "{}";
    tooltip = true;
    on-click = "${np}/bin/nowplay-popup";
    on-click-middle = "${np}/bin/nowplay toggle";
    on-click-right = "${np}/bin/nowplay next";
    on-scroll-up = "${np}/bin/nowplay vol +5";
    on-scroll-down = "${np}/bin/nowplay vol -5";
  };
```

The Nix files have only been syntax-checked, not built.

## Waybar

Merge `contrib/waybar/config.jsonc` into your waybar config and add `"custom/music"` to one of the module lists. Style it with `contrib/waybar/style.css`.

| Action | What it does |
|---|---|
| left click | open the popup (click again to close) |
| middle click | play / pause |
| right click | next track |
| scroll | volume +5 / -5 |

The module shows `<artist> - <title>` with a play or pause icon, and a tooltip with title, artist, album and which browser. With `nowplay waybar --icon-only` the bar shows just the play / pause icon (the tooltip keeps the full info). CSS classes are `playing`, `paused`, `stopped` and `none`.

## Popup placement

- **Built with `--layer-shell`** (Wayland compositors that support it: sway, Hyprland, river, ...): the popup anchors itself and by default is centred directly under a top bar (or over a bottom bar), because the compositor keeps it out of the space the bar reserves. Options: `--anchor top|top-right|top-left|bottom|bottom-right|bottom-left` (default `top`), `--margin-y PX` (gap to the bar) and `--margin-x PX` (distance from the left/right screen edge; ignored for the centred `top` / `bottom`). To put it under a specific module instead of the centre, anchor to a corner and use `--margin-x`, for example `nowplay-popup --anchor top-left --margin-x 560`. On NixOS use the `nowplay-layer-shell` package for this.
- **Plain build**: it is an ordinary window, so make windows with app-id / class `dev.nowplay.popup` floating in your compositor config. For sway: `for_window [app_id="dev.nowplay.popup"] floating enable`. On Hyprland add a float rule for class `dev.nowplay.popup`; the rule syntax has changed between Hyprland versions, so check yours.
- It closes on Esc, on focus loss (disable with `--no-autoclose`), or when you run `nowplay-popup` again. If focus-loss closing does not trigger on your compositor with layer-shell, use Esc, the waybar click, or `--no-autoclose`.

## CLI

```
nowplay status          [playing] Artist - Title (1:23 / 3:45) vol 80%
nowplay toggle | play | pause | stop | next | prev
nowplay vol +5 | -5 | 40
nowplay waybar [--max-len N] [--icon-only]
```

Commands act on the same player the widget would show.

## Browser setup

Both families expose MPRIS by default as far as I know, but I could not test either here.

- **Firefox**: `media.hardwaremediakeys.enabled` in `about:config` should be `true` (the default).
- **Chromium / Chrome / Brave / Vivaldi**: if nothing appears, check that "Hardware media key handling" (`chrome://flags/#hardware-media-key-handling`) is enabled.
- Some browsers only register with MPRIS once a page has started playing media. Run `busctl --user list | grep mpris` to see what is on your bus.

## Known limitations

- **Volume is up to the browser.** The slider writes the MPRIS `Volume` property. I could not verify whether Firefox and Chromium both honour writes to it. If one ignores it, the slider snaps back to the real value about a second after you let go, rather than pretending it worked. If that happens, tell me which browser and I will add a fallback (for example PipeWire per-app volume through `wpctl`).
- **Seeking** uses `SetPosition` and falls back to a relative `Seek`. Some browsers do not allow seeking on some pages (live streams, certain sites), in which case the bar is disabled.
- With two players playing at once, it follows the one it was already showing.
- Anything that is not a browser (Spotify, mpv, VLC) works the same way as long as it exposes MPRIS.

## Layout

```
src/model.rs               pure logic + unit tests (cargo test)
src/mpris.rs               D-Bus client (dbus crate)
src/art.rs                 album art: local files + small download cache
src/bin/nowplay.rs         CLI + waybar feed
src/bin/nowplay-popup/     GTK4 window (main, ui, backend, style.css)
contrib/waybar/            config + css snippets
```

Colours are in `src/bin/nowplay-popup/style.css` (Catppuccin Mocha-style dark theme). Rebuild after editing it.
