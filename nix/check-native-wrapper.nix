{ pkgs }:
let
  lib = pkgs.lib;
  package = pkgs.runCommand "oh-my-ime-native-fixture" { } ''
    mkdir -p $out/bin $out/share/applications $out/share/ibus/component
    printf '#!${pkgs.runtimeShell}\nprintf "%%s\\n" "$OMIME_ENGINE_PATH" "$PATH"\n' > $out/bin/omimed
    chmod +x $out/bin/omimed
    for entry in InputMethod IBus; do
      printf '[Desktop Entry]\nExec=%s/bin/omimed --ibus\n' "$out" > $out/share/applications/io.github.ign1x.OhMyIme.$entry.desktop
    done
    printf '<component><exec>%s/bin/omimed --ibus</exec></component>\n' "$out" > $out/share/ibus/component/oh-my-ime.xml
  '';
  rime = pkgs.runCommand "oh-my-ime-rime-fixture" { } ''mkdir -p $out/share/oh-my-ime/engines'';
  native = import ./native-package.nix {
    inherit pkgs lib package rime;
    frameworks = [ pkgs.coreutils ];
  };
in pkgs.runCommand "oh-my-ime-native-wrapper-check" { } ''
  test "$(${native}/bin/omimed | head -1)" = '${rime}/share/oh-my-ime/engines'
  ${native}/bin/omimed | tail -1 | grep -F '${pkgs.coreutils}/bin'
  grep -F 'Exec=${native}/bin/omimed --ibus' ${native}/share/applications/io.github.ign1x.OhMyIme.InputMethod.desktop
  grep -F '<exec>${native}/bin/omimed --ibus</exec>' ${native}/share/ibus/component/oh-my-ime.xml
  # Rewriting native metadata must not modify the source derivation.
  grep -F '<exec>${package}/bin/omimed --ibus</exec>' ${package}/share/ibus/component/oh-my-ime.xml
  touch $out
''
