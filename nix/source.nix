# Zyris built from source, in the shape nixpkgs asks for (`pkgs/by-name/zy/zyris/package.nix`).
# The flake ships `package.nix`, the repackaged .deb; this one is kept ready for when a nixpkgs
# reviewer asks for a source build. ONNX Runtime comes from nixpkgs through pkg-config, so nothing
# is downloaded at build time. Build it with:
#   nix-build -E 'with import <nixpkgs> {}; callPackage ./nix/source.nix {}'
{
  lib,
  stdenv,
  rustPlatform,
  fetchFromGitHub,
  fetchPnpmDeps,

  cargo-tauri,
  cmake,
  meson,
  ninja,
  nodejs,
  pkg-config,
  pnpm_11,
  pnpmConfigHook,
  wrapGAppsHook3,

  abseil-cpp,
  alsa-lib,
  dbus,
  glib-networking,
  libayatana-appindicator,
  libgbm,
  libxkbcommon,
  onnxruntime,
  openssl,
  pipewire,
  webkitgtk_4_1,

  nix-update-script,
}:

let
  pnpm = pnpm_11;
in
rustPlatform.buildRustPackage (finalAttrs: {
  pname = "zyris";
  version = "0.1.1";

  strictDeps = true;
  __structuredAttrs = true;

  src = fetchFromGitHub {
    owner = "attacca-cc";
    repo = "zyris";
    tag = "v${finalAttrs.version}";
    hash = "sha256-ncVtBFLI1w/KSIV7EBbzZBmO+sR0n7gchJ9yKTVu57A=";
  };

  cargoHash = "sha256-T5oKlKYR1fNeybiVkgUa413od4yFVMYKEd8an8OWIGc=";

  pnpmDeps = fetchPnpmDeps {
    inherit (finalAttrs) pname version src;
    inherit pnpm;
    fetcherVersion = 4;
    hash = "sha256-kUnKehQ7QFtTnMmxb9VoCede5rhK0TEPZAKuWNFWMqQ=";
  };

  # The tray icon is dlopen'd by name.
  postPatch = ''
    substituteInPlace $cargoDepsCopy/*/libappindicator-sys-*/src/lib.rs \
      --replace-fail "libayatana-appindicator3.so.1" \
        "${libayatana-appindicator}/lib/libayatana-appindicator3.so.1"
  '';

  buildAndTestSubdir = "crates/zyris-app";
  # `aec` builds the vendored webrtc-audio-processing with meson, as the release does.
  buildFeatures = [
    "voice"
    "aec"
  ];

  nativeBuildInputs = [
    cargo-tauri.hook
    cmake
    meson
    ninja
    nodejs
    pkg-config
    pnpm
    pnpmConfigHook
    rustPlatform.bindgenHook
    wrapGAppsHook3
  ];

  buildInputs = [
    abseil-cpp
    alsa-lib
    dbus
    glib-networking
    libgbm
    libxkbcommon
    onnxruntime
    openssl
    pipewire
    webkitgtk_4_1
  ];

  # cmake and meson are for whisper.cpp and webrtc-audio-processing inside build scripts;
  # the top-level build is cargo.
  dontUseCmakeConfigure = true;
  dontUseMesonConfigure = true;
  dontUseNinjaBuild = true;
  dontUseNinjaInstall = true;
  dontUseNinjaCheck = true;

  env = {
    OPENSSL_NO_VENDOR = true;
    # whisper.cpp otherwise tunes for the build machine's CPU.
    GGML_NATIVE = "OFF";
  };

  passthru.updateScript = nix-update-script { };

  meta = {
    description = "Desktop node for Attacca that lets an agent talk with you and use this computer";
    homepage = "https://github.com/attacca-cc/zyris";
    changelog = "https://github.com/attacca-cc/zyris/releases/tag/v${finalAttrs.version}";
    license = lib.licenses.asl20;
    maintainers = with lib.maintainers; [ ridanit-ruma ];
    mainProgram = "zyris";
    platforms = lib.platforms.linux;
  };
})
