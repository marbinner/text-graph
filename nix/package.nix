# The text-graph derivation, kept separate from flake.nix so a non-flake NixOS
# config can use it directly:  pkgs.callPackage ./nix/package.nix { }
{
  lib,
  rustPlatform,
  pkg-config,
  makeWrapper,
  tmux,
  xdg-utils,
  libGL,
  libxkbcommon,
  wayland,
  libx11,
  libxcursor,
  libxi,
  libxrandr,
}:

let
  # dlopen'd, never linked: glow/glutin reach for libGL, winit for
  # libxkbcommon, libwayland-client and the X11 libs. These go into the
  # binary's RUNPATH rather than an LD_LIBRARY_PATH wrapper, so they are not
  # inherited by the editors, agents and tmux panes the viewer spawns.
  runtimeLibs = [
    libGL
    libxkbcommon
    wayland
    libx11
    libxcursor
    libxi
    libxrandr
  ];

  # Suffixed onto PATH, so a tmux the user already has keeps precedence: the
  # viewer attaches to their running server, and the editors and agents it
  # spawns must resolve from their environment, not from this closure.
  runtimeBins = [
    tmux
    xdg-utils
  ];
in
rustPlatform.buildRustPackage {
  pname = "text-graph";
  version = "0.3.0"; # keep in step with Cargo.toml

  # Explicit fileset, so editing README/CLAUDE.md/PLAN.md does not rebuild.
  # fixtures/, tests/ and examples/ are here because the check phase needs
  # them: the integration tests reach fixtures/vault via CARGO_MANIFEST_DIR,
  # and --all-targets compiles the examples.
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
      ../assets
      ../fixtures
      ../tests
      ../examples
    ];
  };

  # Every lock entry is crates.io, so no vendor hash to keep in sync. It does
  # vendor wgpu too, which Cargo.toml deselects in favour of glow — a bigger
  # fetch, but nothing extra compiles.
  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [
    pkg-config
    makeWrapper
  ];

  # Not linked against: x11-dl's build script pkg-config-probes the Xorg .pc
  # files and fails the build outright when they are missing.
  buildInputs = [
    libx11
    libxcursor
    libxi
    libxrandr
  ];

  # With tmux present the mirror/typing/resize tests actually run; without it
  # they skip-pass, which is a much weaker gate.
  nativeCheckInputs = [ tmux ];

  # plain `cargo test` skips the examples, and a broken one has shipped before
  cargoTestFlags = [ "--all-targets" ];

  preCheck = ''
    export HOME=$TMPDIR
    export TMUX_TMPDIR=$TMPDIR # the tests' private tmux sockets stay in the sandbox

    # tmux sanitizes non-printables in -F output to '_' unless the client
    # locale is UTF-8, which erases both the 0x1f field separator the scan
    # format relies on and the raw bytes of non-UTF-8 paths. The sandbox sets
    # no locale, so say so explicitly or tmux_mirror's launch tests fail here
    # and nowhere else.
    export LC_ALL=C.UTF-8
  '';

  # patchelf first: wrapProgram renames the real ELF to .text-graph-wrapped.
  # This runs after stdenv's --shrink-rpath, so the added rpath survives.
  postFixup = ''
    patchelf --add-rpath ${lib.makeLibraryPath runtimeLibs} $out/bin/text-graph
    wrapProgram $out/bin/text-graph \
      --suffix PATH : ${lib.makeBinPath runtimeBins}
  '';

  meta = {
    description = "Native graph viewer for a folder of markdown notes, with live tmux agent terminals in the graph";
    homepage = "https://github.com/marbinner/text-graph";
    # license: intentionally unset — the author has not picked one (see Cargo.toml)
    mainProgram = "text-graph";
    platforms = lib.platforms.linux;
  };
}
