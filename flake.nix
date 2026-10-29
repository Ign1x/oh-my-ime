{
  description = "oh-my-ime — a modern, Wayland-first, UI-agnostic input method platform for Linux";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      devShells = forAllSystems (
        pkgs:
        let
          # Libraries that are linked at build time.
          buildLibs = with pkgs; [
            dbus
            libxkbcommon
            wayland
          ];
          # Libraries that toolkits (winit/Slint, Mesa) dlopen at runtime.
          runtimeLibs = with pkgs; [
            fontconfig
            freetype
            libGL
            libxkbcommon
            vulkan-loader
            wayland
          ];
        in
        {
          default = pkgs.mkShell {
            packages =
              buildLibs
              ++ (with pkgs; [
                pkg-config
                cargo-deny
                # Compositors and tools for end-to-end and manual testing.
                sway
                hyprland
                foot
                grim
                wlrctl
                wtype
                jq
              ]);

            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;

            shellHook = ''
              # The host session may point GTK/Qt at fcitx; test sessions must use
              # text-input-v3 so oh-my-ime actually sees the traffic.
              unset GTK_IM_MODULE QT_IM_MODULE XMODIFIERS
            '';
          };
        }
      );

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
