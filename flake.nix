{
  description = "telearia2 - Manage aria2 with telegram bot";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { nixpkgs, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
        };
        package = pkgs.rustPlatform.buildRustPackage {
          pname = "telearia2";
          version = "0.1.10";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = [
            pkgs.pkg-config
          ];

          buildInputs = [
            pkgs.openssl
          ];

          meta = with pkgs.lib; {
            description = "Manage aria2 with telegram bot";
            homepage = "https://github.com/ihciah/telearia2";
            license = licenses.mit;
            mainProgram = "telearia2";
          };
        };
      in
      {
        packages.default = package;

        apps.default = flake-utils.lib.mkApp {
          drv = package;
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            clippy
            rustc
            rustfmt
            pkg-config
            openssl
          ];

          shellHook = ''
            export RUST_BACKTRACE=1
          '';
        };

        formatter = pkgs.nixfmt-rfc-style;
      });
}
