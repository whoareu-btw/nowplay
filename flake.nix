{
  description = "nowplay: tiny MPRIS music controller (waybar module + dark popup)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: rec {
        # waybar module + remote + popup window (plain window)
        nowplay = pkgs.callPackage ./nix/package.nix { };
        default = nowplay;

        # same, but the popup anchors itself under your bar (Wayland layer-shell)
        nowplay-layer-shell = nowplay.override { withLayerShell = true; };

        # only the waybar module / remote: no GTK, smallest closure
        nowplay-cli = nowplay.override { withGui = false; };
      });

      overlays.default = final: _prev: {
        nowplay = final.callPackage ./nix/package.nix { };
      };

      # Deliberately independent of the package: you need this shell to create
      # Cargo.lock in the first place (`nix develop -c cargo generate-lockfile`).
      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            cargo
            rustc
            rustfmt
            clippy
            pkg-config
          ];
          buildInputs = with pkgs; [
            dbus
            glib
            gtk4
            gtk4-layer-shell
          ];
          shellHook = ''
            export XDG_DATA_DIRS="$XDG_DATA_DIRS:${pkgs.gsettings-desktop-schemas}/share/gsettings-schemas/${pkgs.gsettings-desktop-schemas.name}:${pkgs.gtk4}/share/gsettings-schemas/${pkgs.gtk4.name}"
          '';
        };
      });
    };
}
