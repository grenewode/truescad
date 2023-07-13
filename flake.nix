{

  nixConfig = {
    substituters = [
      "https://nix-community.cachix.org"
      "https://rust-analyzer-flake.cachix.org"
    ];

    trusted-public-keys = [
      "rust-analyzer-flake.cachix.org-1:M0/jTcCtgtFl6/aZV4l08+JN9Zf5dHzALWrKmCXeeoU="
      "nix-community.cachix.org-1:mB9FSh9qf2dCimDSUo8Zy7bkq5CX+/rkCWyvRCYg3Fs="

    ];
  };

  inputs = {
    flake-utils.url = "github:numtide/flake-utils";
    naersk.url = "github:nmattia/naersk";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs = { self, nixpkgs, flake-utils, naersk, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let

        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        inherit (pkgs)
          pkg-config alsa-lib udev wayland wayland-protocols libxkbcommon xorg
          vulkan-loader rust-analyzer cmake glib cairo atk pango gdk-pixbuf gtk3
          gtksourceview3;

        inherit (pkgs.lib) makeLibraryPath optional;

        inherit (pkgs.rust-bin) fromRustupToolchainFile;

        rust = fromRustupToolchainFile ./rust-toolchain.toml;

        naersk-lib = naersk.lib."${system}".override {
          cargo = rust;
          rustc = rust;
        };

        inherit (naersk-lib) buildPackage;

        buildInputs = [
          alsa-lib
          udev

          # Gtk libraries
          gtk3
          gdk-pixbuf
          gtksourceview3

          # Gnome Libaries
          glib
          cairo
          atk
          pango

          # Vulcan Support
          vulkan-loader

          # Wayland Support
          wayland
          wayland-protocols
          libxkbcommon

          # X11 Support
          xorg.libX11
          xorg.libXcursor
          xorg.libXi
          xorg.libXrandr
          xorg.libXinerama
          xorg.libXxf86vm
        ];

        nativeBuildInputs = [ pkg-config cmake ];

        package = buildPackage {
          root = ./.;
          inherit buildInputs nativeBuildInputs;
        };
        name = (builtins.parseDrvName package.name).name;
      in rec {
        # `nix build`
        packages.${name} = package;
        packages.default = package;
        defaultPackage = package;

        # `nix run`
        apps.${name} = flake-utils.lib.mkApp { drv = packages.default; };
        apps.default = apps.${name};
        defaultApp = apps.default;

        # `nix develop`
        devShell = (pkgs.mkShell {
          inputsFrom = optional (builtins.pathExists ./Cargo.lock) package;

          inherit nativeBuildInputs buildInputs;

          packages = [ rust rust-analyzer ];

          shellHook = let ldLibraryPath = makeLibraryPath buildInputs;
          in ''
            export LD_LIBRARY_PATH="''${LD_LIBRARY_PATH}:${ldLibraryPath}"
          '';
        });
      });
}
