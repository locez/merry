{
  lib,
  stdenv,
  rustPlatform,
  pkg-config,
  oniguruma,
  wayland,
  libxkbcommon,
}:

let
  cargoToml = builtins.fromTOML (builtins.readFile ../Cargo.toml);
in
rustPlatform.buildRustPackage {
  pname = "merry";
  version = cargoToml.workspace.package.version;

  src = lib.cleanSource ../.;

  cargoLock.lockFile = ../Cargo.lock;

  cargoBuildFlags = [
    "--package"
    "merry-cli"
  ];
  doCheck = false;

  nativeBuildInputs = [ pkg-config ];
  buildInputs = [
    oniguruma
  ]
  ++ lib.optionals stdenv.hostPlatform.isLinux [
    wayland
    libxkbcommon
  ];

  # Only the `merry` binary is installed from the workspace. nixpkgs passes an
  # explicit target triple, so the release directory is platform-qualified.
  installPhase = ''
    runHook preInstall
    install -Dm755 "$(find target -type f -path '*/release/merry' -print -quit)" "$out/bin/merry"
    runHook postInstall
  '';

  meta = {
    description = "A lightweight terminal coding agent (CLI & interactive TUI)";
    homepage = "https://github.com/locez/merry";
    license = with lib.licenses; [
      asl20
      mit
    ];
    mainProgram = "merry";
    platforms = [
      "aarch64-darwin"
      "aarch64-linux"
      "x86_64-darwin"
      "x86_64-linux"
    ];
  };
}
