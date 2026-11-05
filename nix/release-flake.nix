# Published as flake.nix inside oh-my-ime-nix.tar.gz, together with release.json
# and the reusable modules. Both package defaults download pinned release assets.
{
  description = "oh-my-ime release binaries and NixOS/Home Manager modules";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  outputs =
    { self, nixpkgs }:
    let
      release = builtins.fromJSON (builtins.readFile ./nix/release.json);
      forAllSystems =
        f:
        nixpkgs.lib.genAttrs (builtins.attrNames release.artifacts) (
          system: f nixpkgs.legacyPackages.${system}
        );
    in
    {
      packages = forAllSystems (pkgs: rec {
        oh-my-ime = pkgs.callPackage ./nix/package.nix { };
        default = oh-my-ime;
        rime = pkgs.callPackage ./nix/rime.nix { };
        full = pkgs.symlinkJoin {
          name = "oh-my-ime-with-rime-${oh-my-ime.version}";
          paths = [
            oh-my-ime
            rime
          ];
        };
      });
      overlays.default = final: _prev: {
        oh-my-ime = final.callPackage ./nix/package.nix { };
        oh-my-ime-rime = final.callPackage ./nix/rime.nix { };
      };
      nixosModules.default = ./nix/nixos.nix;
      homeManagerModules.default = ./nix/home-manager.nix;
      lib.version = release.version;
      checks = forAllSystems (pkgs: {
        platform = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        rime = self.packages.${pkgs.stdenv.hostPlatform.system}.rime;
      });
    };
}
