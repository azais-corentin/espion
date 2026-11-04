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
      url = "github:tangency-labs/solidcraft/v0.26.1";
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

          # SolidCraft's modelling skills for omp. The shellHook links them from the locked
          # `solidcraft` input into .omp/skills (gitignored), so they always match the
          # solidcraft-cli above. omp reads skills at startup: restart it after a bump.
          solidcraftSkills = [
            "solidcraft-modeling"
            "solidcraft-editing"
            "solidcraft-checking"
            "solidcraft-mechanisms"
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
              # Git hooks (hk.pkl): Conventional Commits checked with cocogitto (cog.toml).
              pkgs.hk
              pkgs.cocogitto
            ];

            KICAD_LIBRARY_TOOLS = "${kicad-library-tools}";

            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath guiLibs;

            shellHook = ''
              if root=$(git rev-parse --show-toplevel 2>/dev/null) \
                && grep -qs 'tangency-labs/solidcraft' "$root/flake.nix"; then
                skills="$root/.omp/skills"
                wanted=" ${pkgs.lib.concatStringsSep " " solidcraftSkills} "
                # Drop links this hook made for skills no longer in the list.
                for link in "$skills"/solidcraft-*; do
                  if [ -L "$link" ] && [[ $(readlink "$link") == /nix/store/* ]] \
                    && [[ $wanted != *" ''${link##*/} "* ]]; then
                    rm "$link"
                  fi
                done
                for name in $wanted; do
                  target="${solidcraft}/skills/$name"
                  link="$skills/$name"
                  if [ -e "$link" ] && [ ! -L "$link" ]; then
                    echo "espion: $link is not a symlink; not linking the SolidCraft skill" >&2
                  elif [ "$(readlink "$link")" != "$target" ]; then
                    ln -sfn "$target" "$link"
                  fi
                done
              fi

              # Install the hk hooks (in .git/config, shared by every worktree) when hk or
              # hk.pkl changed since the last install, so parallel shells don't all rewrite
              # .git/config.
              if root=$(git rev-parse --show-toplevel 2>/dev/null) && [ -f "$root/hk.pkl" ]; then
                stamp="$(git -C "$root" rev-parse --path-format=absolute --git-common-dir)/hk-install.stamp"
                want="${pkgs.hk} $(sha256sum < "$root/hk.pkl")"
                if [ "$(cat "$stamp" 2>/dev/null)" != "$want" ] \
                  || ! git -C "$root" config --get-regexp '^hook\.hk-' >/dev/null; then
                  if out=$(cd "$root" && hk install 2>&1); then
                    printf '%s\n' "$want" > "$stamp"
                  else
                    printf '%s\n' "$out" >&2
                    echo "espion: hk install failed; the commit hooks are not installed" >&2
                  fi
                fi
              fi
            '';
          };
        }
      );

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
