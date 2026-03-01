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

      homeManagerModules.default = { config, lib, pkgs, ... }:
        let
          cfg = config.services.mean-feeder;
          feedsFile = pkgs.writeText "mean-feeder-feeds" (lib.concatStringsSep "\n" cfg.feeds);
          noisyFeedsFile = pkgs.writeText "mean-feeder-noisy-feeds" (lib.concatStringsSep "\n" cfg.noisyFeeds);
        in {
          options.services.mean-feeder = {
            enable = lib.mkEnableOption "mean-feeder";
            port = lib.mkOption {
              type = lib.types.port;
              default = 3101;
              description = "Port to listen on";
            };
            dataDir = lib.mkOption {
              type = lib.types.str;
              default = "${config.xdg.dataHome}/mean-feeder";
              description = "Directory for storing feed data";
            };
            feeds = lib.mkOption {
              type = lib.types.listOf lib.types.str;
              default = [];
              description = "List of RSS/Atom feed URLs";
            };
            noisyFeeds = lib.mkOption {
              type = lib.types.listOf lib.types.str;
              default = [];
              description = "List of high-volume RSS/Atom feed URLs shown in a separate section";
            };
            pageSize = lib.mkOption {
              type = lib.types.int;
              default = 25;
              description = "Number of entries per page";
            };
            fetchInterval = lib.mkOption {
              type = lib.types.int;
              default = 3600;
              description = "Seconds between feed refreshes";
            };
          };

          config = lib.mkIf cfg.enable {
            systemd.user.services.mean-feeder = {
              Unit = {
                Description = "RSS Reader";
                After = [ "network-online.target" ];
              };
              Service = {
                ExecStart = "${mean-feeder}/bin/mean-feeder";
                Restart = "on-failure";
                RestartSec = 5;
                WorkingDirectory = cfg.dataDir;
                Environment = [
                  "PORT=${toString cfg.port}"
                  "FETCH_INTERVAL=${toString cfg.fetchInterval}"
                  "PAGE_SIZE=${toString cfg.pageSize}"
                ] ++ lib.optional (cfg.feeds != [])
                  "FEEDS_FILE=${feedsFile}"
                ++ lib.optional (cfg.noisyFeeds != [])
                  "NOISY_FEEDS_FILE=${noisyFeedsFile}";
              };
              Install = {
                WantedBy = [ "default.target" ];
              };
            };

            home.activation.mean-feederData = lib.hm.dag.entryAfter [ "writeBoundary" ] ''
              mkdir -p "${cfg.dataDir}"
            '';
          };
        };

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
