{
  cmake,
  llvmPackages,
  openssl,
  libcap ? null,
  rustPlatform,
  pkg-config,
  lib,
  stdenv,
  fetchurl,
  version ? "0.0.0",
  ...
}:
let
  # The `v8` crate (pinned "=149.2.0" in the workspace Cargo.toml, pulled in
  # via code-mode) downloads a prebuilt static V8 in its build.rs, which the
  # Nix sandbox forbids. Prefetch it and point RUSTY_V8_ARCHIVE at it — same
  # approach nixpkgs uses for Deno. Bump version + hashes together.
  v8Version = "149.2.0";
  librusty_v8 =
    let
      byPlatform = {
        aarch64-darwin = { target = "aarch64-apple-darwin";      hash = "sha256-+rsuyNO6Wm3qY9uaNalg3FypheujLzQrm6Sqocc0sv4="; };
        x86_64-darwin  = { target = "x86_64-apple-darwin";       hash = "sha256-eUlAo4o/ZrfvUqXwA8awlPdDrQQKZK+z082frUlADwc="; };
        aarch64-linux  = { target = "aarch64-unknown-linux-gnu"; hash = "sha256-+XdRJ8pk3MSjZi0BpSGizvuluY+DOUOog9hHc7Kv88U="; };
        x86_64-linux   = { target = "x86_64-unknown-linux-gnu";  hash = "sha256-iu2YY323533Iv7i7R1nsW95HLQv3lD9Y4OYqNQlFxVk="; };
      };
      p = byPlatform.${stdenv.hostPlatform.system};
    in
    fetchurl {
      url = "https://github.com/denoland/rusty_v8/releases/download/v${v8Version}/librusty_v8_release_${p.target}.a.gz";
      inherit (p) hash;
    };
in
rustPlatform.buildRustPackage (_: {
  env.PKG_CONFIG_PATH = lib.makeSearchPathOutput "dev" "lib/pkgconfig" (
    [ openssl ] ++ lib.optionals stdenv.isLinux [ libcap ]
  );
  env.RUSTY_V8_ARCHIVE = librusty_v8;
  pname = "codex-rs";
  inherit version;
  cargoLock.lockFile = ./Cargo.lock;
  doCheck = false;
  src = ./.;

  # Patch the workspace Cargo.toml so that cargo embeds the correct version in
  # CARGO_PKG_VERSION (which the binary reads via env!("CARGO_PKG_VERSION")).
  # On release commits the Cargo.toml already contains the real version and
  # this sed is a no-op.
  postPatch = ''
    sed -i 's/^version = "0\.0\.0"$/version = "${version}"/' Cargo.toml
  '';
  nativeBuildInputs = [
    cmake
    llvmPackages.clang
    llvmPackages.libclang.lib
    openssl
    pkg-config
  ] ++ lib.optionals stdenv.isLinux [
    libcap
  ];

  cargoLock.outputHashes = {
    "ratatui-0.29.0" = "sha256-HBvT5c8GsiCxMffNjJGLmHnvG77A6cqEL+1ARurBXho=";
    "crossterm-0.28.1" = "sha256-6qCtfSMuXACKFb9ATID39XyFDIEMFDmbx6SSmNe+728=";
    "nucleo-0.5.0" = "sha256-Hm4SxtTSBrcWpXrtSqeO0TACbUxq3gizg1zD/6Yw/sI=";
    "nucleo-matcher-0.3.1" = "sha256-Hm4SxtTSBrcWpXrtSqeO0TACbUxq3gizg1zD/6Yw/sI=";
    "runfiles-0.1.0" = "sha256-uJpVLcQh8wWZA3GPv9D8Nt43EOirajfDJ7eq/FB+tek=";
    "tokio-tungstenite-0.28.0" = "sha256-V1xmnrfRWOcZZogelZEA4vvyMj2awCfHVA5/glQ6KAI=";
    "tungstenite-0.27.0" = "sha256-VVHhk7l9J/sEmG3q/UuV/sQ3f+fGsmq5vumSy8vbMvw=";
  };

  meta = with lib; {
    description = "OpenAI Codex command‑line interface rust implementation";
    license = licenses.asl20;
    homepage = "https://github.com/openai/codex";
    mainProgram = "codex";
  };
})
