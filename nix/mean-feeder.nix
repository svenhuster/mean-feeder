{ config, lib, pkgs, ... }:

let
  cfg = config.services.mean-feeder;

  mean-feeder = pkgs.rustPlatform.buildRustPackage {
    pname = "mean-feeder";
    version = "0.1.0";
    src = ../.;
    cargoLock.lockFile = ../Cargo.lock;
  };

  instanceModule = { name, ... }: {
    options = {
      enable = lib.mkEnableOption "mean-feeder instance '${name}'";

      port = lib.mkOption {
        type = lib.types.port;
        default = 3101;
        description = "Port for the web server.";
      };

      feeds = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [];
        description = "List of RSS/Atom feed URLs.";
      };

      noisyFeeds = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [];
        description = "List of high-volume RSS/Atom feed URLs shown in a separate section.";
      };

      pageSize = lib.mkOption {
        type = lib.types.int;
        default = 25;
        description = "Number of entries per page.";
      };

      fetchInterval = lib.mkOption {
        type = lib.types.int;
        default = 3600;
        description = "Seconds between feed refreshes.";
      };
    };
  };

  mkServices = name: icfg:
    let
      feedsFile = pkgs.writeText "mean-feeder-${name}-feeds" (lib.concatStringsSep "\n" icfg.feeds);
      noisyFeedsFile = pkgs.writeText "mean-feeder-${name}-noisy-feeds" (lib.concatStringsSep "\n" icfg.noisyFeeds);
    in
    lib.optionalAttrs icfg.enable {
      "mean-feeder-${name}" = {
        description = "mean-feeder RSS reader (${name})";
        wantedBy = [ "multi-user.target" ];
        after = [ "network.target" ];
        serviceConfig = {
          Type = "simple";
          DynamicUser = true;
          StateDirectory = "mean-feeder/${name}";
          WorkingDirectory = "/var/lib/mean-feeder/${name}";
          Environment = [
            "PORT=${toString icfg.port}"
            "FETCH_INTERVAL=${toString icfg.fetchInterval}"
            "PAGE_SIZE=${toString icfg.pageSize}"
          ] ++ lib.optional (icfg.feeds != [])
            "FEEDS_FILE=${feedsFile}"
          ++ lib.optional (icfg.noisyFeeds != [])
            "NOISY_FEEDS_FILE=${noisyFeedsFile}";
          ExecStart = "${mean-feeder}/bin/mean-feeder";
          Restart = "on-failure";
          RestartSec = 5;
        };
      };
    };
in
{
  options.services.mean-feeder = {
    instances = lib.mkOption {
      type = lib.types.attrsOf (lib.types.submodule instanceModule);
      default = {};
      description = "Named mean-feeder instances.";
    };
  };

  config = {
    systemd.services = lib.mkMerge (lib.mapAttrsToList mkServices cfg.instances);
  };
}
