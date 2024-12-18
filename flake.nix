{
  description = "The Erlang Language Platform";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs =
    { self, nixpkgs, rust-overlay, ... }:
    let
      inherit (nixpkgs) lib;
      forEachSystem = lib.genAttrs lib.systems.flakeExposed;
    in
    {
      packages = forEachSystem (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };
          erlangPkgs = pkgs.beam.packages.erlang_26;
          rebar3 = erlangPkgs.rebar3.overrideAttrs (final: prev: { doCheck = false; });
        in
        rec {
          default = elp;
          elp = pkgs.rustPlatform.buildRustPackage {
            pname = "elp";
            version = "devel";
            src = self;

            cargoLock = {
              lockFile = ./Cargo.lock;
              outputHashes = {
                "paths-0.0.0" = "sha256-gAVkwyyKvmL4O3E3MGTgSILQ6jjbNFjY7Yfp9Df/fCM=";
              };
            };

            nativeBuildInputs = [ erlangPkgs.erlang rebar3 ];

            # Skip the eqwalizer build script.
            ELP_EQWALIZER_SKIP = "1";

            doCheck = false;
          };
        }
      );
      devShell = forEachSystem (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };
          erlangPkgs = pkgs.beam.packages.erlang_26;
          rebar3 = erlangPkgs.rebar3.overrideAttrs (final: prev: { doCheck = false; });
          toolchain = pkgs.rust-bin.stable.latest.default;
        in
        pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            (toolchain.override {
              extensions = [
                "rust-src"
                "rustfmt"
              ];
            })
            rebar3
            rust-analyzer
            cargo-flamegraph
            valgrind
            erlang_26
            clang
          ];
          RUST_BACKTRACE = "1";
          # Skip the eqwalizer build script.
          ELP_EQWALIZER_SKIP = "1";
        }
      );
    };
}
