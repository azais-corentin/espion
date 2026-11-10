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
    # Ferrite EDA: its web viewer publishes the camera board's schematic, PCB and 3D board
    # (packages.<system>.espion-cam-viewer, deployed to GitHub Pages by
    # .github/workflows/viewer.yml). It keeps its own nixpkgs: the viewer bundle
    # is built with Ferrite's pinned toolchain.
    ferrite.url = "github:azais-corentin/ferrite";
  };

  outputs =
    {
      nixpkgs,
      rust-overlay,
      solidcraft,
      kicad-library-tools,
      ferrite,
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
      # The web viewer site of hardware/espion-cam: Ferrite's viewer bundle plus the
      # KiCad project and the project-local libraries in hardware/lib it references, with
      # the board and its parts' 3D models baked at build time for the 3D view. Stock KiCad
      # models come from nixpkgs' kicad.libraries.packages3d (this flake's pkgs).
      packages = forAllSystems (pkgs: {
        espion-cam-viewer = ferrite.lib.mkProjectSite {
          inherit pkgs;
          name = "espion-cam";
          src = pkgs.lib.fileset.toSource {
            root = ./hardware;
            fileset = pkgs.lib.fileset.unions [
              ./hardware/espion-cam
              ./hardware/lib
            ];
          };
          entry = "espion-cam/espion-cam.kicad_pro";
        };
      });

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

          # MCP server that lets omp drive a running egui app (click, type, read the widget
          # tree, screenshot) over eframe's `inspection` feature: .omp/mcp.json starts it.
          # Its egui version must match the workspace's eframe.
          eguiMcp =
            (pkgs.makeRustPlatform {
              cargo = rust;
              rustc = rust;
            }).buildRustPackage
              rec {
                pname = "egui_mcp";
                version = "0.3.0";
                src = pkgs.fetchCrate {
                  inherit pname version;
                  hash = "sha256-zKqVwK4ksDz0yP1nRazUvq5FlXE9GzWmk6O5hjO82N4=";
                };
                cargoHash = "sha256-fyoS6pu/EBg44rKwqgApVAzIuZ3v0MbLYlHNmKmatEo=";
                # The tests render egui's demo app with wgpu.
                doCheck = false;
              };

          # Runs a GUI command without a desktop: in cage on wlroots' headless backend, with
          # software rendering (Mesa's lavapipe) and eframe's inspection port open, so an
          # agent can start an egui app and drive it through the egui MCP server. The
          # compositor lives in its own runtime directory, away from the user's session.
          eguiHeadless = pkgs.writeShellApplication {
            name = "egui-headless";
            runtimeInputs = [
              pkgs.cage
              pkgs.wlr-randr
              pkgs.coreutils
              pkgs.util-linux
            ];
            text = ''
              usage() {
                echo "usage: egui-headless [-s WIDTHxHEIGHT] [-p PORT] COMMAND [ARG...]" >&2
                echo "  -s  size of the headless output, which the window fills (default 1280x800)" >&2
                echo "  -p  port of eframe's inspection server on 127.0.0.1 (default 5719)" >&2
                exit 64
              }
              size=1280x800
              port=5719
              while getopts s:p: opt; do
                case $opt in
                  s) size=$OPTARG ;;
                  p) port=$OPTARG ;;
                  *) usage ;;
                esac
              done
              shift $((OPTIND - 1))
              [ $# -ge 1 ] || usage

              runtime=$(mktemp -d)
              trap 'rm -rf "$runtime"' EXIT
              export XDG_RUNTIME_DIR=$runtime
              unset DISPLAY WAYLAND_DISPLAY
              export WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1
              export VK_DRIVER_FILES=${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.${pkgs.stdenv.hostPlatform.parsed.cpu.name}.json
              export EGUI_INSPECTION=127.0.0.1:$port
              # Compositor and application in their own session, so that stopping this
              # script stops both.
              # shellcheck disable=SC2016
              setsid cage -- sh -c 'wlr-randr --output HEADLESS-1 --custom-mode "$0" && exec "$@"' "$size" "$@" &
              group=$!
              trap 'kill -TERM -- "-$group" 2>/dev/null || true' TERM INT HUP
              status=0
              wait "$group" || status=$?
              # A trapped signal ends the first wait early.
              if kill -0 "$group" 2>/dev/null; then
                status=0
                wait "$group" || status=$?
              fi
              exit "$status"
            '';
          };

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
              eguiMcp
              eguiHeadless
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
