# /etc/profile.d/claw-cursor.sh — fallback path for setting the default
# cursor theme.
#
# /etc/environment is normally sufficient (PAM exports it into every
# session), but some shell-launched contexts (`startx`-style or
# `cosmic-session` invoked outside greetd) bypass PAM. Sourcing this
# script from /etc/profile gives those paths the same defaults.
#
# Users override via ~/.config/cosmic/com.clawos.Tk/v1/... once the
# Settings page learns to write a cursor_theme key, or by editing this
# file in their own dotfiles.

: "${XCURSOR_THEME:=Bibata-Modern-Classic}"
: "${XCURSOR_SIZE:=24}"
export XCURSOR_THEME XCURSOR_SIZE
