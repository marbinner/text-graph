{
  description = "Native graph viewer for a folder of markdown notes, with live tmux agent terminals in the graph";

  # nixpkgs is the only input: no flake-utils, the eachSystem helper below is
  # four lines and costs nothing to lock.
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      # Linux only — the tmux control-mode client and the rustix bits are Unix,
      # and nobody has run this anywhere else.
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system nixpkgs.legacyPackages.${system});
    in
    {
      overlays.default = final: _prev: {
        text-graph = final.callPackage ./nix/package.nix { };
      };

      packages = forAllSystems (
        _system: pkgs: rec {
          text-graph = pkgs.callPackage ./nix/package.nix { };
          default = text-graph;
        }
      );

      apps = forAllSystems (
        system: _pkgs: rec {
          text-graph = {
            type = "app";
            program = nixpkgs.lib.getExe self.packages.${system}.text-graph;
          };
          default = text-graph;
        }
      );

      devShells = forAllSystems (
        system: pkgs: {
          default = pkgs.mkShell {
            # inherits pkg-config and the Xorg libs from the package
            inputsFrom = [ self.packages.${system}.default ];

            packages = with pkgs; [
              cargo
              rustc
              rustfmt
              clippy
              rust-analyzer
              cargo-audit
              tmux
            ];

            # A dev shell is the one place LD_LIBRARY_PATH is the right tool:
            # `cargo run` builds a binary nothing has patchelf'd.
            LD_LIBRARY_PATH = nixpkgs.lib.makeLibraryPath (
              with pkgs;
              [
                libGL
                libxkbcommon
                wayland
                libx11
                libxcursor
                libxi
                libxrandr
              ]
            );
          };
        }
      );

      formatter = forAllSystems (_system: pkgs: pkgs.nixfmt-tree);
    };
}
