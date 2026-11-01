-- oh-my-ime for Hyprland (Lua configuration, Hyprland 0.55 and later).
-- Load it from ~/.config/hypr/hyprland.lua, or copy the parts you want.

-- Start the daemon with Hyprland.
hl.on("hyprland.start", function()
    hl.exec_cmd("omimed")
end)

-- How applications find omimed: Qt 6.8 and later and GTK through the compositor's
-- text-input, Qt 5 and Qt on X11 (WeChat, WPS) through the Fcitx5 module, which omimed
-- serves, and other X11 applications through XIM. System Qt applications need the
-- fcitx5-qt plugins for the module; applications that bundle Qt bring their own.
hl.env("GTK_IM_MODULE", "")
hl.env("QT_IM_MODULE", "fcitx")
hl.env("QT_IM_MODULES", "wayland;fcitx")
hl.env("XMODIFIERS", "@im=fcitx")

-- Optional: switch engines from anywhere, not only inside text fields.
-- (omimed's own hotkeys, Ctrl+Space and Ctrl+Shift+Space, work while typing.)
hl.bind("SUPER + space", hl.dsp.exec_cmd("omime next"))
