-- oh-my-ime for Hyprland (Lua configuration, Hyprland 0.55 and later).
-- Load it from ~/.config/hypr/hyprland.lua, or copy the parts you want.

-- Start the daemon with Hyprland.
hl.on("hyprland.start", function()
    hl.exec_cmd("omimed")
end)

-- Applications should talk to the compositor (text-input-v3), not to an IM module.
hl.env("GTK_IM_MODULE", "")
hl.env("QT_IM_MODULE", "")

-- Optional: switch engines from anywhere, not only inside text fields.
-- (omimed's own hotkeys, Ctrl+Space and Ctrl+Shift+Space, work while typing.)
hl.bind("SUPER + space", hl.dsp.exec_cmd("omime next"))
