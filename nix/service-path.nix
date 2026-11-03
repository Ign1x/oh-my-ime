# The PATH of omimed's unit, for both modules: the hosted frameworks first, then where
# programs usually are, the user's profiles included. A unit's PATH replaces the user
# manager's, and engines' programs (manifests run them by name) may be anywhere there.
# systemd expands %h and %u in the unit.
{ lib, frameworks }:
lib.concatStringsSep ":" (
  lib.optional (frameworks != [ ]) (lib.makeBinPath frameworks)
  ++ [
    "/run/wrappers/bin"
    "%h/.nix-profile/bin"
    "/etc/profiles/per-user/%u/bin"
    "/nix/var/nix/profiles/default/bin"
    "/run/current-system/sw/bin"
    "/usr/local/bin"
    "/usr/bin"
    "/bin"
  ]
)
