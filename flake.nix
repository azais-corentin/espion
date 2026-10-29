{
  description = "espion: multi-camera IR marker tracking (ESP32-P4 firmware, Rust host, KiCad hardware)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # SolidCraft CAD; its MCP server (solidcraft-cli mcp) builds the 3D models in hardware/lib.
    solidcraft = {
      url = "github:tangency-labs/solidcraft/v0.25.1";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # KiCad library tooling (KLC check scripts in library_utils/klc-check, symbol and
    # footprint generators), used by the KiCad toolkit in .omp/extensions/kicad.
    # kicad-library-utils was merged into this repo; library_utils/ may still move.
    kicad-library-tools = {
      url = "git+https://gitlab.com/kicad/libraries/kicad-library-tools.git";
      flake = false;
    };
  };

  outputs =
    {
      nixpkgs,
      rust-overlay,
      solidcraft,
      kicad-library-tools,
      ...
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems =
        f:
        nixpkgs.lib.genAttrs systems (
          system:
          f (
            import nixpkgs {
              inherit system;
              overlays = [ rust-overlay.overlays.default ];
            }
          )
        );
    in
    {
      devShells = forAllSystems (
        pkgs:
        let
          # Host + riscv32imafc (ESP32-P4), pinned in one place.
          rust = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

          # Loaded at runtime by eframe/winit (espion-viewer).
          guiLibs = with pkgs; [
            libGL
            libxkbcommon
            vulkan-loader
            wayland
            libx11
            libxcursor
            libxi
            libxrandr
          ];
        in
        {
          default = pkgs.mkShell {
            packages = [
              rust
              pkgs.espflash
              pkgs.probe-rs-tools
              pkgs.kicad
              solidcraft.packages.${pkgs.stdenv.hostPlatform.system}.solidcraft
              pkgs.pkg-config
              # KiCad toolkit (.omp/extensions/kicad): bun runs its tests, Python the KLC
              # scripts and the official symbol/footprint generators.
              pkgs.bun
              (pkgs.python3.withPackages (
                ps: with ps; [
                  asteval
                  colorlog
                  pyclipper
                  pyyaml
                  tabulate
                  typing-extensions
                ]
              ))
              # Datasheets: pdftoppm/pdftotext (kicad_datasheet) and image crops.
              pkgs.poppler-utils
              pkgs.imagemagick
            ];

            KICAD_LIBRARY_TOOLS = "${kicad-library-tools}";

            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath guiLibs;
          };
        }
      );

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
