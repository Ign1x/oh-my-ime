# KWin and IBus launch desktop/component commands outside the omimed unit. Carry
# engine paths on that command too, without installing hosted frameworks' autostarts.
{ pkgs, lib, package, frameworks, rime ? null }:
pkgs.symlinkJoin {
  name = "${package.pname or "oh-my-ime"}-native-${package.version or "0"}";
  paths = [ package ];
  nativeBuildInputs = [ pkgs.makeWrapper ];
  postBuild = ''
    rm $out/bin/omimed
    makeWrapper ${package}/bin/omimed $out/bin/omimed \
      ${lib.optionalString (frameworks != [ ]) ''--prefix PATH : ${lib.escapeShellArg (lib.makeBinPath frameworks)}''} \
      ${lib.optionalString (rime != null) ''--prefix OMIME_ENGINE_PATH : ${rime}/share/oh-my-ime/engines''}
    for entry in io.github.ign1x.OhMyIme.InputMethod.desktop io.github.ign1x.OhMyIme.IBus.desktop; do
      source=$out/share/applications/$entry
      if [ -f "$source" ]; then
        substitute "$source" "$TMPDIR/$entry" \
          --replace 'Exec=${package}/bin/omimed' "Exec=$out/bin/omimed" \
          --replace 'Exec=omimed' "Exec=$out/bin/omimed"
        mv "$TMPDIR/$entry" "$source"
      fi
    done
    source=$out/share/ibus/component/oh-my-ime.xml
    substitute "$source" "$TMPDIR/oh-my-ime.xml" \
      --replace '<exec>${package}/bin/omimed --ibus</exec>' "<exec>$out/bin/omimed --ibus</exec>" \
      --replace '<exec>omimed --ibus</exec>' "<exec>$out/bin/omimed --ibus</exec>"
    mv "$TMPDIR/oh-my-ime.xml" "$source"
  '';
  meta = (package.meta or { }) // { isIbusEngine = true; };
}
