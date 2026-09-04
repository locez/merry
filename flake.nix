{
  description = "Merry development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
  };

  outputs =
    { nixpkgs, ... }:
    let
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];

      forEachSystem =
        function:
        nixpkgs.lib.genAttrs systems (
          system:
          function (
            import nixpkgs {
              inherit system;
            }
          )
        );
    in
    {
      devShells = forEachSystem (
        pkgs:
        let
          shellPackages = [
            pkgs.cargo
            pkgs.clippy
            pkgs.git
            pkgs.maturin
            pkgs.nodejs_22
            pkgs.nixfmt
            pkgs.pkg-config
            pkgs.python312
            pkgs.ripgrep
            pkgs.rust-analyzer
            pkgs.rustc
            pkgs.rustfmt
            pkgs.stdenv.cc
            pkgs.uv
          ]
          ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
            pkgs.bubblewrap
          ];
        in
        {
          default = pkgs.mkShell {
            packages = shellPackages;

            CARGO_TERM_COLOR = "always";
            PYO3_PYTHON = "${pkgs.python312}/bin/python3";
            RUST_BACKTRACE = "1";
            UV_PYTHON = "${pkgs.python312}/bin/python3";

            shellHook = ''
              export PATH="${pkgs.lib.makeBinPath shellPackages}:$PATH"
              export CC="${pkgs.stdenv.cc}/bin/cc"
              export CXX="${pkgs.stdenv.cc}/bin/c++"
            '';
          };
        }
      );
    };
}
