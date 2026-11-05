# Local/CI validation uses the exact archives that will be uploaded, before a
# Release exists. Example: nix build --impure --file nix/check-release.nix
# --argstr artifacts /absolute/path/to/dist --no-link
{
  artifacts,
  system ? builtins.currentSystem,
}:
let
  flake = builtins.getFlake ("git+file://" + toString ../.);
  pkgs = import flake.inputs.nixpkgs { inherit system; };
  directory = builtins.toPath artifacts;
  manifest = builtins.fromJSON (builtins.readFile (directory + "/${system}.json"));
  package =
    component:
    pkgs.callPackage ./binary.nix {
      inherit (manifest) version;
      src = pkgs.fetchurl {
        url = "file://${
          builtins.path {
            path = directory + "/${manifest.artifacts.${component}.file}";
            name = manifest.artifacts.${component}.file;
          }
        }";
        inherit (manifest.artifacts.${component}) hash;
      };
      rime = component == "rime";
    };
in
pkgs.symlinkJoin {
  name = "oh-my-ime-release-check-${manifest.version}";
  paths = [
    (package "platform")
    (package "rime")
  ];
}
