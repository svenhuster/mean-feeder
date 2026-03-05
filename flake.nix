{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };

      mean-feeder = pkgs.rustPlatform.buildRustPackage {
        pname = "mean-feeder";
        version = "0.1.0";
        src = ./.;
        cargoLock.lockFile = ./Cargo.lock;
      };
    in {
      packages.${system}.default = mean-feeder;

      nixosModules.mean-feeder = import ./nix/mean-feeder.nix;

      devShells.${system}.default = pkgs.mkShell {
        buildInputs = [
          pkgs.cargo
          pkgs.rustc
          pkgs.rustfmt
          pkgs.clippy
          pkgs.rust-analyzer
        ];
        FEEDS_FILE = pkgs.writeText "dev-feeds" "https://lobste.rs/rss";
        NOISY_FEEDS_FILE = pkgs.writeText "dev-noisy-feeds" "https://hnrss.org/frontpage";
      };
    };
}
