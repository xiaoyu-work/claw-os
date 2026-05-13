# Auto-start X on tty1 once the live user has logged in. This avoids
# pulling in a full display manager (lightdm / gdm pull ~200MB of deps).
#
# Only runs:
#   - on tty1 (not on ttyS0 serial console, not on SSH, not on tty2+)
#   - when DISPLAY is unset (so we don't recurse into another X session)
#   - when XDG_VTNR is unset OR is "1" (covers consoles that don't set tty)

if [ -z "${DISPLAY:-}" ] && [ "$(tty 2>/dev/null)" = "/dev/tty1" ]; then
    echo "Starting Claw OS installer..."
    exec startx /etc/cos/installer-xstartup -- vt1 >/dev/null 2>&1
fi
