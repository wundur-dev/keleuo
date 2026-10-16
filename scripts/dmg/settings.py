# The installer window's layout, read by dmgbuild (see scripts/make-dmg, which passes the
# app and the background with -D). Positions are the icons' centres, in points from the top
# left, the same as in background.swift, which draws the arrow between them.

app = defines["app"]  # noqa: F821 (given by dmgbuild)
background = defines["background"]  # noqa: F821

files = [app]
symlinks = {"Applications": "/Applications"}

format = "ULMO"  # LZMA: the smallest image; opens on macOS 10.15 and later
filesystem = "HFS+"

window_rect = ((200, 140), (660, 420))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False

icon_size = 112
text_size = 13
icon_locations = {
    "Keleuo.app": (170, 190),
    "Applications": (490, 190),
}
