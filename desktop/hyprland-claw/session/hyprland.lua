hl.monitor({ output = "", mode = "preferred", position = "auto", scale = "auto" })

hl.config({
    misc = {
        disable_hyprland_logo = true,
        disable_splash_rendering = true,
        -- The Claw OS supervisor owns compositor lifetime and the OS owns dialogs.
        disable_watchdog_warning = true,
        disable_hyprland_guiutils_check = true,
    },
})

hl.bind("SUPER + Q", hl.dsp.window.close())
for _, direction in ipairs({ "left", "right", "up", "down" }) do
    hl.bind("SUPER + " .. direction, hl.dsp.focus({ direction = direction }))
end
for workspace = 1, 9 do
    hl.bind("SUPER + " .. workspace, hl.dsp.focus({ workspace = workspace }))
    hl.bind("SUPER + SHIFT + " .. workspace, hl.dsp.window.move({ workspace = workspace }))
end
