"""Material 3 look for the app window.

Colour roles follow the M3 tonal scheme (blue seed, matching the icon), as
CSS custom properties on `window.m3`, with a dark variant. libadwaita's own
variables are remapped to them so stock widgets (header bars, boxed lists,
dialogs) blend in. Loaded above USER priority so a ~/.config/gtk-4.0 theme
doesn't flatten it; every rule is scoped to `window.m3` or to our own classes.
"""

CSS = """
window.m3 {
  --m3-primary: #0b57d0;
  --m3-on-primary: #ffffff;
  --m3-primary-container: #d3e3fd;
  --m3-on-primary-container: #041e49;
  --m3-secondary-container: #c2e7ff;
  --m3-on-secondary-container: #004a77;
  --m3-error: #b3261e;
  --m3-error-container: #f9dedc;
  --m3-on-error-container: #410e0b;
  --m3-success: #146c2e;
  --m3-warning: #8a5300;
  --m3-surface: #f8fafd;
  --m3-surface-container-low: #f0f4f9;
  --m3-surface-container: #e9eef6;
  --m3-surface-container-high: #dde3ea;
  --m3-surface-container-highest: #d3d9e0;
  --m3-on-surface: #1f1f1f;
  --m3-on-surface-variant: #444746;
  --m3-outline: #747775;
  --m3-outline-variant: #c4c7c5;
}
window.m3.dark {
  --m3-primary: #a8c7fa;
  --m3-on-primary: #062e6f;
  --m3-primary-container: #0842a0;
  --m3-on-primary-container: #d3e3fd;
  --m3-secondary-container: #004a77;
  --m3-on-secondary-container: #c2e7ff;
  --m3-error: #f2b8b5;
  --m3-error-container: #8c1d18;
  --m3-on-error-container: #f9dedc;
  --m3-success: #6dd58c;
  --m3-warning: #ffba20;
  --m3-surface: #131314;
  --m3-surface-container-low: #1b1b1c;
  --m3-surface-container: #1e1f20;
  --m3-surface-container-high: #282a2c;
  --m3-surface-container-highest: #333537;
  --m3-on-surface: #e3e3e3;
  --m3-on-surface-variant: #c4c7c5;
  --m3-outline: #8e918f;
  --m3-outline-variant: #444746;
}

/* Stock widgets inside the window. */
window.m3 {
  --window-bg-color: var(--m3-surface);
  --window-fg-color: var(--m3-on-surface);
  --view-bg-color: var(--m3-surface);
  --headerbar-bg-color: var(--m3-surface);
  --headerbar-shade-color: transparent;
  --card-bg-color: var(--m3-surface-container);
  --accent-bg-color: var(--m3-primary);
  --accent-fg-color: var(--m3-on-primary);
  --accent-color: var(--m3-primary);
  background-color: var(--m3-surface);
  color: var(--m3-on-surface);
}
window.m3 headerbar { background-color: var(--m3-surface); box-shadow: none; }
window.m3 list.boxed-list {
  background-color: var(--m3-surface-container);
  border-radius: 20px;
  border: none;
  box-shadow: none;
}
window.m3 list.boxed-list > row { background-color: transparent; }
window.m3 button.suggested-action {
  background-color: var(--m3-primary);
  background-image: none;
  color: var(--m3-on-primary);
  box-shadow: none;
  border: none;
}
window.m3 button.suggested-action:hover {
  background-color: color-mix(in srgb, var(--m3-primary), var(--m3-on-primary) 8%);
}
window.m3 levelbar block.filled { background-color: var(--m3-primary); }

/* Header. */
.hero-icon { -gtk-icon-shadow: 0 4px 12px rgb(0 0 0 / 0.18); }
.hero-status .icon { -gtk-icon-shadow: 0 6px 16px rgb(0 0 0 / 0.18); }
.m3-headline { font-size: 1.75em; font-weight: 500; }
.m3-chip {
  border: 1px solid var(--m3-outline-variant);
  border-radius: 8px;
  padding: 5px 12px 5px 10px;
  color: var(--m3-on-surface-variant);
  font-size: 0.9em;
  font-weight: 500;
}
.status-dot { min-width: 8px; min-height: 8px; border-radius: 999px; background-color: var(--m3-outline); }
.status-dot.ok { background-color: var(--m3-success); }
.status-dot.warn { background-color: var(--m3-warning); }
.status-dot.bad { background-color: var(--m3-error); }
.m3-section { color: var(--m3-primary); font-weight: 600; font-size: 0.95em; }

/* Service tiles: filled cards; primary container when on. */
button.service-tile {
  padding: 16px;
  border-radius: 24px;
  border: none;
  box-shadow: none;
  background-image: none;
  background-color: var(--m3-surface-container-high);
  color: var(--m3-on-surface);
  outline-offset: 2px;
  transition: background-color 200ms ease-out;
}
button.service-tile:hover {
  background-color: color-mix(in srgb, var(--m3-surface-container-high), var(--m3-on-surface) 8%);
}
button.service-tile:active {
  background-color: color-mix(in srgb, var(--m3-surface-container-high), var(--m3-on-surface) 12%);
}
button.service-tile.on { background-color: var(--m3-primary-container); color: var(--m3-on-primary-container); }
button.service-tile.on:hover {
  background-color: color-mix(in srgb, var(--m3-primary-container), var(--m3-on-primary-container) 8%);
}
button.service-tile:disabled { opacity: 0.5; }
.service-tile .tile-icon {
  min-width: 40px;
  min-height: 40px;
  border-radius: 999px;
  background-color: var(--m3-surface-container-highest);
  color: var(--m3-on-surface-variant);
}
.service-tile.on .tile-icon { background-color: var(--m3-primary); color: var(--m3-on-primary); }
.service-tile .tile-state { color: var(--m3-on-surface-variant); }
.service-tile.on .tile-state { color: inherit; opacity: 0.8; }

/* M3 switch drawn inside the tile (the whole tile is the toggle). */
.m3-switch {
  min-width: 44px;
  min-height: 24px;
  padding: 0 4px;
  border-radius: 999px;
  border: 2px solid var(--m3-outline);
  background-color: var(--m3-surface-container-highest);
  transition: all 200ms ease-out;
}
.m3-switch > .knob {
  min-width: 12px;
  min-height: 12px;
  border-radius: 999px;
  background-color: var(--m3-outline);
  transition: all 200ms ease-out;
}
.service-tile.on .m3-switch { background-color: var(--m3-primary); border-color: var(--m3-primary); padding: 0 1px; }
.service-tile.on .m3-switch > .knob { min-width: 20px; min-height: 20px; background-color: var(--m3-on-primary); }

/* Face cards: outlined cards; the add card is dashed. */
menubutton.face-card > button, button.face-card {
  padding: 16px 8px 14px;
  border-radius: 20px;
  border: 1px solid var(--m3-outline-variant);
  box-shadow: none;
  background-image: none;
  background-color: var(--m3-surface-container-low);
  color: var(--m3-on-surface);
}
menubutton.face-card > button:hover, button.face-card:hover {
  background-color: color-mix(in srgb, var(--m3-surface-container-low), var(--m3-on-surface) 8%);
}
button.face-card.add-card {
  border: 2px dashed var(--m3-outline-variant);
  background-color: transparent;
  color: var(--m3-primary);
}
.face-card avatar {
  background-image: none;
  background-color: var(--m3-primary-container);
  color: var(--m3-on-primary-container);
}
.badge {
  padding: 2px 8px;
  border-radius: 8px;
  font-size: 0.8em;
  font-weight: 600;
  background-color: var(--m3-secondary-container);
  color: var(--m3-on-secondary-container);
}
.badge.warning { background-color: var(--m3-error-container); color: var(--m3-on-error-container); }

/* Filled button. */
button.test-button { min-height: 48px; padding: 0 28px; border-radius: 999px; font-weight: 600; }

/* Round icon used as a row prefix (test page, settings). */
.tile-icon { min-width: 36px; min-height: 36px; border-radius: 999px; }
.tile-icon.accent { background-color: var(--m3-primary); color: var(--m3-on-primary); }
.tile-icon.success { background-color: var(--m3-success); color: var(--m3-surface); }
.tile-icon.error { background-color: var(--m3-error); color: var(--m3-surface); }
.tile-icon.neutral { background-color: var(--m3-surface-container-highest); color: var(--m3-on-surface-variant); }
"""
