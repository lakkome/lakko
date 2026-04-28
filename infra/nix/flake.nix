# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors

{
  description = "Lakko — pinned development toolchain (Rust, build tools, protobuf, lint)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        # Pinned via rust-toolchain.toml at the repo root.
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ../../rust-toolchain.toml;
      in {
        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            rustToolchain

            # Build / dev tooling
            just
            cargo-deny
            cargo-audit
            cargo-fuzz
            cargo-mutants
            reuse

            # Protobuf
            protobuf

            # Native deps that may be needed by future crates
            pkg-config
            openssl

            # Git hooks
            pre-commit
          ];

          shellHook = ''
            echo "Lakko dev shell — Rust $(rustc --version)"
            echo "Run \`just\` for available recipes."
          '';
        };
      });
}
