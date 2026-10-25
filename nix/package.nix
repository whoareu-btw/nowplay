{
  lib,
  rustPlatform,
  pkg-config,
  dbus,
  glib,
  gtk4,
  gtk4-layer-shell,
  wrapGAppsHook4,
  # false = only the `nowplay` waybar module / remote (no GTK at all)
  withGui ? true,
  # true = popup anchors itself under the bar on Wayland (needs layer-shell support)
  withLayerShell ? false,
}:

assert lib.assertMsg (builtins.pathExists ../Cargo.lock) ''

  Cargo.lock is missing. Nix needs it to build offline. Create it once:

    nix develop -c cargo generate-lockfile
    git add Cargo.lock        # flakes only see files git knows about

  then run your nix command again.
'';
assert lib.assertMsg (withLayerShell -> withGui) "withLayerShell needs withGui";

rustPlatform.buildRustPackage {
  pname = "nowplay";
  version = "0.1.0";

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [ pkg-config ] ++ lib.optionals withGui [ wrapGAppsHook4 ];

  buildInputs =
    [ dbus ]
    ++ lib.optionals withGui [
      glib
      gtk4
    ]
    ++ lib.optional withLayerShell gtk4-layer-shell;

  # Without the GUI only `nowplay` is built (the popup binary needs the `gui` feature).
  buildNoDefaultFeatures = !withGui;
  buildFeatures = lib.optional withLayerShell "layer-shell";

  # Wrap only the popup (it needs GTK schemas/pixbuf loaders). The waybar
  # binary stays a plain executable, with no wrapper process in front of it.
  dontWrapGApps = true;
  preFixup = lib.optionalString withGui ''
    wrapProgram $out/bin/nowplay-popup \
      "''${gappsWrapperArgs[@]}" ${
        # gtk4-layer-shell has to be loaded before libwayland-client; preloading
        # it is the documented workaround and is harmless if it was not needed.
        lib.optionalString withLayerShell "--prefix LD_PRELOAD : ${lib.getLib gtk4-layer-shell}/lib/libgtk4-layer-shell.so"
      }
  '';

  meta = {
    description = "Tiny MPRIS music controller: waybar module + dark popup for Firefox/Chromium";
    license = lib.licenses.mit;
    mainProgram = "nowplay";
    platforms = lib.platforms.linux;
  };
}
