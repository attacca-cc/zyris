# The development shell: `nix develop`, or direnv's `use flake`. Everything `cargo build` and
# `pnpm tauri dev` need, with the `voice` and `aec` features included. The libraries follow
# `source.nix`; the runtime bits follow what `package.nix` patches into the released binary.
{
  lib,
  mkShell,
  rustPlatform,

  cargo,
  rustc,
  rustfmt,
  clippy,
  rust-analyzer,
  cargo-tauri,
  nodejs,
  pnpm_11,

  pkg-config,
  cmake,
  meson,
  ninja,

  abseil-cpp,
  alsa-lib,
  dbus,
  glib,
  glib-networking,
  gsettings-desktop-schemas,
  gtk3,
  libayatana-appindicator,
  libgbm,
  libGL,
  libsoup_3,
  libxkbcommon,
  onnxruntime,
  openssl,
  pipewire,
  vulkan-loader,
  wayland,
  webkitgtk_4_1,
  xorg,
}:

mkShell {
  nativeBuildInputs = [
    cargo
    rustc
    rustfmt
    clippy
    rust-analyzer
    cargo-tauri
    nodejs
    pnpm_11

    pkg-config
    cmake
    meson
    ninja
    # libclang for the bindgen in pipewire's, whisper.cpp's and webrtc-audio-processing's -sys crates.
    rustPlatform.bindgenHook
  ];

  buildInputs = [
    abseil-cpp
    alsa-lib
    dbus
    glib
    glib-networking
    gtk3
    libayatana-appindicator
    libgbm
    libsoup_3
    libxkbcommon
    onnxruntime
    openssl
    pipewire
    wayland
    webkitgtk_4_1
    xorg.libX11
    xorg.libxcb
    xorg.libXext
    xorg.libXfixes
    xorg.libXi
    xorg.libXrandr
    xorg.libXtst
  ];

  env = {
    OPENSSL_NO_VENDOR = "1";
    # whisper.cpp otherwise tunes for this CPU, and `zyris-voice`'s build.rs refuses a release.
    GGML_NATIVE = "OFF";
    # Loaded with dlopen: the tray, and the graphics the webview and the GPU backends reach for.
    LD_LIBRARY_PATH = lib.makeLibraryPath [
      libayatana-appindicator
      libGL
      vulkan-loader
    ];
    GIO_EXTRA_MODULES = "${glib-networking}/lib/gio/modules";
  };

  # What `wrapGAppsHook3` does for the package: without the GSettings schemas, WebKitGTK on
  # Wayland lays the page out at a zoom of -1/96.
  shellHook = ''
    export XDG_DATA_DIRS="${gsettings-desktop-schemas}/share/gsettings-schemas/${gsettings-desktop-schemas.name}:${gtk3}/share/gsettings-schemas/${gtk3.name}''${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}"
  '';
}
