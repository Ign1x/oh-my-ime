#!/usr/bin/env python3
"""Native GNOME Shell -> IBus -> oh-my-ime -> Rime -> Wayland text-input probe."""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import runpy
import shutil
import sys
import time
import traceback
from xml.sax.saxutils import escape

ROOT = Path(__file__).resolve().parents[2]
shared = runpy.run_path(str(ROOT / "scripts/desktop-probe.py"))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--shell', default='gnome-shell')
    parser.add_argument('--bin-dir', type=Path, default=ROOT/'target/debug')
    parser.add_argument('--gtk3', action='store_true',
                        help='also type into a real GTK3 Gtk.Entry in the private Shell session')
    parser.add_argument('--gtk4', action='store_true',
                        help='also type into a real GTK4 Gtk.Entry in the private Shell session')
    args = parser.parse_args()
    args.out = args.out.resolve()
    args.bin_dir = args.bin_dir.resolve()
    args.out.mkdir(parents=True, exist_ok=False)
    env = shared['private_environment'](args.out)
    env.update(PATH=str(args.bin_dir.resolve())+os.pathsep+env['PATH'],
               XDG_CURRENT_DESKTOP='GNOME', XDG_SESSION_DESKTOP='gnome',
               WAYLAND_DISPLAY='omime-gnome-test', OMIME_ENGINE_PATH=str(ROOT/'engines'),
               GSETTINGS_BACKEND='keyfile', OMIME_LOG='debug', IBUS_ADDRESS=f'unix:path={args.out}/run/ibus',
               IBUS_COMPONENT_PATH=str(args.out/'components'))
    components = args.out/'components'
    components.mkdir()
    shutil.copy(ROOT/'data/ibus/oh-my-ime.xml', components/'oh-my-ime.xml')
    keyfile = Path(env['XDG_CONFIG_HOME'])/'glib-2.0/settings'
    keyfile.mkdir(parents=True)
    (keyfile/'keyfile').write_text("[org/gnome/desktop/input-sources]\nsources=[('ibus', 'oh-my-ime')]\nmru-sources=[('ibus', 'oh-my-ime')]\n[org/gnome/shell]\nwelcome-dialog-last-shown-version='999'\n")
    session = shared['Session'](args.out, env)
    report = {'desktop':'GNOME Shell','checks':{},
              'scope':'headless Shell with real IBus, Rime and Wayland editor',
              'at_utc': datetime.now(timezone.utc).isoformat(),
              'revision': shared['version'](['git','-C',str(ROOT),'rev-parse','HEAD']),
              'worktree_dirty': shared['worktree_dirty'](),
              'probe_script_sha256': shared['fingerprint'](Path(__file__).resolve()),
              'binary_sha256': {name: shared['fingerprint'](args.bin_dir/name) for name in
                               ('omimed','omime-rime','omime-ti-client')},
              'shell_version': shared['version']([args.shell,'--version']),
              'limitations': ['Headless virtual output; physical displays, mixed DPI, fullscreen and launchers not tested.',
                              'Native synthetic Wayland text field; GTK/Qt/browser applications need separate acceptance.']}
    gtk_toolkits = [version for version, enabled in (('GTK3', args.gtk3), ('GTK4', args.gtk4)) if enabled]
    if gtk_toolkits:
        report['scope'] += ' and ' + ' and '.join(f'a real {version} Gtk.Entry' for version in gtk_toolkits)
        untested = [version for version in ('GTK3', 'GTK4') if version not in gtk_toolkits]
        untested += ['Qt', 'browser', 'Electron', 'office']
        report['limitations'][1] = ('Native synthetic Wayland text field; ' + ', '.join(untested) +
                                    ' applications need separate acceptance.')
    remote = None
    try:
        bus = args.out/'bus.conf'
        bus.write_text('<busconfig><type>session</type><listen>'+escape(env['DBUS_SESSION_BUS_ADDRESS'])+'</listen><auth>EXTERNAL</auth><policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
        session.start('dbus',['dbus-daemon','--nofork',f'--config-file={bus}'])
        shared['wait_for']('private bus',lambda:(args.out/'run/bus').exists())
        session.start('ibus',['ibus-daemon','--single','--panel','disable','--config','disable','--emoji-extension','disable','--cache','none','--address',env['IBUS_ADDRESS']])
        shared['wait_for']('private IBus',lambda:(args.out/'run/ibus').exists())
        session.start('ibus-wire',['dbus-monitor','--address',env['IBUS_ADDRESS']])
        config=Path(env['XDG_CONFIG_HOME'])/'oh-my-ime'
        config.mkdir()
        (config/'config.toml').write_text('engines = ["rime"]\n[panel]\nenabled = false\n[tray]\nenabled = false\n')
        session.start('omimed',[args.bin_dir/'omimed','--ibus','--engine','rime'])
        shared['wait_for']('Rime schemas',lambda:'rime/luna_pinyin' in session.run(['omime','list','--all']).stdout,60)
        session.run(['omime','switch','rime/luna_pinyin'])
        shell = session.start('shell', [args.shell, '--wayland', '--headless', '--no-x11',
                                        '--virtual-monitor', '1280x720', '--wayland-display',
                                        env['WAYLAND_DISPLAY']],
                             MUTTER_DEBUG_FORCE_VIRTUAL='1', LIBGL_ALWAYS_SOFTWARE='1')
        shared['wait_for']('GNOME display',lambda:(Path(env['XDG_RUNTIME_DIR'])/env['WAYLAND_DISPLAY']).exists(),30)
        import gi
        from gi.repository import Gio, GLib
        connection = Gio.DBusConnection.new_for_address_sync(env['DBUS_SESSION_BUS_ADDRESS'],Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT|Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,None,None)
        def call(destination,path,interface,method,signature=None,values=(), bus=None):
            return (bus or connection).call_sync(destination,path,interface,method,GLib.Variant(signature,values) if signature else None,None,Gio.DBusCallFlags.NONE,5000,None)
        def ready():
            try:return call('org.freedesktop.DBus','/org/freedesktop/DBus','org.freedesktop.DBus','NameHasOwner','(s)',('org.gnome.Mutter.RemoteDesktop',)).unpack()[0]
            except GLib.Error:return False
        shared['wait_for']('Shell input service',ready,30)
        remote = call('org.gnome.Mutter.RemoteDesktop','/org/gnome/Mutter/RemoteDesktop','org.gnome.Mutter.RemoteDesktop','CreateSession').unpack()[0]
        call('org.gnome.Mutter.RemoteDesktop',remote,'org.gnome.Mutter.RemoteDesktop.Session','Start')
        def key(code):
            for pressed in [True,False]:
                call('org.gnome.Mutter.RemoteDesktop',remote,'org.gnome.Mutter.RemoteDesktop.Session','NotifyKeyboardKeycode','(ub)',(code,pressed));time.sleep(.04)
        time.sleep(2)
        # GNOME opens overview on startup. Escape returns to the desktop.
        key(1)
        session.start('editor',[args.bin_dir/'omime-ti-client'])
        log=args.out/'editor.log'
        shared['wait_for']('editor',lambda:log.exists() and 'ready' in log.read_text())
        time.sleep(1)
        report['checks']['shell_startup']='passed'
        # Select the registered engine on the private IBus session.
        ibus_connection = Gio.DBusConnection.new_for_address_sync(
            env['IBUS_ADDRESS'], Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT |
            Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
        # IBus's CLI additionally deserializes its GlobalEngine property and can
        # report exit 1 after SetGlobalEngine has already succeeded. Use the actual
        # method reply and ensuing composition to accept the selection.
        call('org.freedesktop.IBus','/org/freedesktop/IBus','org.freedesktop.IBus',
             'SetGlobalEngine','(s)',('oh-my-ime',),bus=ibus_connection)
        report['checks']['ibus_engine_selection']='passed'
        time.sleep(.5)
        keys=dict(zip('qwertyuiop',range(16,26)))|dict(zip('asdfghjkl',range(30,39)))|dict(zip('zxcvbnm',range(44,51)))|{' ':57}
        for toolkit in gtk_toolkits:
            gtk_id = toolkit.lower()
            # Keep the event log separate from Session.start's stdout/stderr log.
            gtk_log = args.out/f'{gtk_id}-entry.log'
            gtk_process = session.start(f'{gtk_id}-editor',
                                        ["python3", ROOT/f'tests/desktop/{gtk_id}-ibus-editor.py'],
                                        OMIME_GTK_LOG=str(gtk_log), GDK_BACKEND='wayland',
                                        GSK_RENDERER='cairo', GTK_A11Y='none',
                                        **({'GTK_IM_MODULE': 'ibus'} if gtk_id == 'gtk3' else {}))
            shared['wait_for'](f'{toolkit} editor',
                               lambda: gtk_log.exists() and 'ready true' in gtk_log.read_text(), 20)
            # `present()` and `grab_focus()` return before GTK has delivered the
            # focus-in event to IBus. Give the toolkit one main-loop turn before
            # injecting the first key, otherwise that key can bypass IBus.
            time.sleep(1)
            for ch in 'zhongguo':
                key(keys[ch])
            key(keys[' '])
            shared['wait_for'](f'{toolkit} Chinese commit',
                               lambda: 'text "中国"' in gtk_log.read_text(), 20)
            if gtk_log.read_text().count('text "中国"') != 1:
                raise RuntimeError(f'{toolkit} received Chinese text more than once')
            shared['wait_for'](f'{toolkit} editor exit', lambda: gtk_process.poll() is not None, 10)
            if gtk_process.returncode != 0:
                raise RuntimeError(f'{toolkit} editor exited with status {gtk_process.returncode}')
            report['checks'][f'{gtk_id}_ibus_commit'] = 'passed'
        if gtk_toolkits:
            # A toolkit owns the active focus after its commit. Start a fresh
            # native text-input client so the remaining protocol checks do not
            # depend on compositor-specific focus restoration behavior.
            session.start('native-editor', [args.bin_dir/'omime-ti-client', '--app-id',
                                            'omime-gnome-native-editor'])
            log = args.out/'native-editor.log'
            shared['wait_for']('native editor after GTK', lambda:log.exists() and 'ready' in log.read_text(), 20)
            time.sleep(.5)
        # Refresh the fixture's cursor after Shell created its IBus context. A
        # direct space and backspace are real editing events, so the fixture sends
        # the latest cursor rectangle through text-input-v3 again.
        key(57)
        key(14)
        shared['wait_for']('baseline direct editing',lambda:'text ""' in log.read_text(),10)
        for ch in 'zhongguo':key(keys[ch])
        shared['wait_for']('inline preedit',lambda:'preedit "zhong' in log.read_text(),10)
        report['checks']['inline_preedit']='passed'
        wire=args.out/'ibus-wire.log'
        shared['wait_for']('native candidate lookup',lambda:'string "中国"' in wire.read_text(),10)
        report['checks']['native_candidate_delivery']='passed'
        # The private no-activation bus has no media-key daemon. Owning this name
        # lets the probe use Shell's normal screenshot caller allowlist without
        # changing Shell's runtime safety mode or reaching the real user session.
        call('org.freedesktop.DBus','/org/freedesktop/DBus','org.freedesktop.DBus',
             'RequestName','(su)',('org.gnome.SettingsDaemon.MediaKeys',0))
        time.sleep(.3)
        try:
            shot=call('org.gnome.Shell.Screenshot','/org/gnome/Shell/Screenshot',
                      'org.gnome.Shell.Screenshot','Screenshot','(bbs)',
                      (False,False,str(args.out/'candidates.png'))).unpack()
            if not shot[0]:raise RuntimeError(f'Shell screenshot failed: {shot}')
            report['checks']['candidate_screenshot']='passed'
            report['candidate_screenshot']=str(args.out/'candidates.png')
            report['candidate_visual_review']='not-tested'
        except GLib.Error as exc:
            report['checks']['candidate_screenshot']='blocked'
            report['candidate_screenshot_error']=str(exc)
        key(keys[' '])
        shared['wait_for']('Chinese commit in native Wayland field',lambda:'commit "中国"' in log.read_text(),20)
        if log.read_text().count('commit "中国"') != 1:
            raise RuntimeError('Chinese phrase committed more than once')
        report['checks']['native_wayland_rime_commit']='passed'
        # Change focus with a live composition, then type into a new native field.
        # Only that field's hao may be committed; ni from the first must not leak.
        for ch in 'ni':key(keys[ch])
        shared['wait_for']('composition before focus change',lambda:'preedit "ni"' in log.read_text(),10)
        session.start('focus-editor',[args.bin_dir/'omime-ti-client','--app-id','omime-gnome-focus-test'])
        focus_log=args.out/'focus-editor.log'
        shared['wait_for']('second editor focus',lambda:focus_log.exists() and 'ready' in focus_log.read_text())
        time.sleep(.5)
        for ch in 'hao ':key(keys[ch])
        shared['wait_for']('second field commit',lambda:'commit "好"' in focus_log.read_text(),10)
        if 'commit "你好"' in focus_log.read_text():raise RuntimeError('composition leaked across focus')
        report['checks']['focus_cleanup']='passed'
        # Password metadata travels through Mutter/Shell to IBus, while keys reach
        # the native application directly and no engine preedit/candidates appear.
        session.start('password-editor',[args.bin_dir/'omime-ti-client','--purpose','8',
                                        '--app-id','omime-gnome-password-test'])
        password_log=args.out/'password-editor.log'
        shared['wait_for']('password editor focus',lambda:password_log.exists() and 'ready' in password_log.read_text())
        time.sleep(.5)
        for ch in 'secret':key(keys[ch])
        shared['wait_for']('password direct typing',lambda:'text "secret"' in password_log.read_text(),10)
        password_events=password_log.read_text().splitlines()
        if any(line.startswith('commit ') for line in password_events):
            raise RuntimeError('engine committed into password field')
        if any(line.startswith('preedit ') and not line.startswith('preedit "" ') for line in password_events):
            raise RuntimeError('engine preedit reached password field')
        report['checks']['password_bypass']='passed'
        if shell.poll() is not None:raise RuntimeError('GNOME Shell exited')
        report['status']='passed'
    except Exception as exc:
        report.update(status='failed',detail=f'{type(exc).__name__}: {exc}',traceback=traceback.format_exc())
    finally:
        if remote:
            try:
                call('org.gnome.Mutter.RemoteDesktop',remote,
                     'org.gnome.Mutter.RemoteDesktop.Session','Stop')
            except Exception:
                pass
        session.close()
        (args.out/'report.json').write_text(json.dumps(report,indent=2,ensure_ascii=False)+'\n')
    print(json.dumps(report,ensure_ascii=False))
    return 0 if report['status']=='passed' else 1

if __name__=='__main__':sys.exit(main())
