{
  description = "Zyris — node runtime, capability catalogue, and reference implementations";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      devShells = forAllSystems (
        pkgs:
        let
          inherit (pkgs) lib stdenv;

          # Everything below exists for one Cargo feature: `zyris-capkit/screen`. The default
          # workspace build needs none of it, which is why that feature is off by default and why
          # CI can stay a plain `cargo build --workspace`.
          #
          # `xcap` reaches the screen four different ways depending on the session, and links
          # against all of them regardless of which one runs:
          linuxCaptureLibs = with pkgs; [
            libxcb # X11 / XRandR
            libxrandr # monitor enumeration on X11
            wayland # wl_output + zwlr_screencopy, the wlroots path
            libgbm # buffer allocation behind that path
            libglvnd # EGL, same
            dbus # the xdg-desktop-portal path
            pipewire # and the stream it hands back
            libxkbcommon
          ];

          # `pipewire-sys` runs bindgen, which dlopens libclang at build time rather than linking
          # it. It is deliberately not `clang` — adding that would make clang the C compiler for
          # the whole shell, which nothing here wants.
          libclang = pkgs.llvmPackages.libclang.lib;

          shellFor =
            extra:
            pkgs.mkShell {
              nativeBuildInputs = [ pkgs.pkg-config ] ++ extra;
              buildInputs = lib.optionals stdenv.isLinux linuxCaptureLibs;

              LIBCLANG_PATH = lib.optionalString stdenv.isLinux "${libclang}/lib";

              # The linker finds these through `buildInputs`; the *test binaries* do not, because
              # nothing writes an rpath for a `cargo test` artifact. Without this, `--features screen`
              # compiles and then dies at startup on `libgbm.so.1`.
              LD_LIBRARY_PATH = lib.optionalString stdenv.isLinux (lib.makeLibraryPath linuxCaptureLibs);
            };
        in
        {
          # Uses whatever `cargo` is already on PATH — rustup, most likely. Keeps one target
          # directory and one toolchain rather than rebuilding the world on every `nix develop`.
          default = shellFor [ ];

          # For a machine with no Rust at all.
          full = shellFor (
            with pkgs;
            [
              cargo
              rustc
              rustfmt
              clippy
              rust-analyzer
            ]
          );
        }
      );
    };
}
