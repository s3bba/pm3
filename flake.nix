{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs = {
        nixpkgs.follows = "nixpkgs";
      };
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      forAllSystems =
        systems: fn:
        let
          overlays = [ (import rust-overlay) ];
        in
        nixpkgs.lib.genAttrs systems (
          system:
          fn system (
            import nixpkgs {
              inherit system overlays;
            }
          )
        );
      devSystems = [
        "x86_64-linux"
        "aarch64-darwin"
      ];
      packageSystems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
    in
    {
      devShells = forAllSystems devSystems (
        _: pkgs: {
          default = pkgs.mkShell {
            buildInputs = [
              pkgs.fzf
              pkgs.pnpm
              pkgs.just
              pkgs.bacon
              pkgs.nodejs
              pkgs.openssl
              pkgs.cargo-dist
              pkgs.pkg-config
              pkgs.rust-analyzer
              pkgs.rust-bin.stable.latest.default
            ];
          };
        }
      );

      packages = forAllSystems packageSystems (
        system: pkgs: {
          pm3 = pkgs.rustPlatform.buildRustPackage {
            pname = cargoToml.package.name;
            version = cargoToml.package.version;
            src = ./.;

            cargoLock = {
              lockFile = ./Cargo.lock;
            };

            nativeBuildInputs = [ pkgs.pkg-config ];
            buildInputs = [ pkgs.openssl ];

            doCheck = false;
          };

          default = self.packages.${system}.pm3;
        }
      );

      apps = forAllSystems packageSystems (
        system: _: {
          pm3 = {
            type = "app";
            program = "${self.packages.${system}.pm3}/bin/pm3";
          };

          default = {
            type = "app";
            program = "${self.packages.${system}.pm3}/bin/pm3";
          };
        }
      );

      formatter = forAllSystems devSystems (
        _: pkgs:
        pkgs.treefmt.withConfig {
          runtimeInputs = [ pkgs.nixfmt-rfc-style ];
          settings = {
            on-unmatched = "info";
            formatter.nixfmt = {
              command = "nixfmt";
              includes = [ "*.nix" ];
            };
          };
        }
      );
    };
}
