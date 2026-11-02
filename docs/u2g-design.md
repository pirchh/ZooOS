# UI step 2g (U2g): dock magnification

Status: implemented 2026-11-02 (version 0.69.0). The owner wants "the
apple feel, extremely polished". Magnification is the Mac dock's
signature: icons near the pointer swell, and their neighbours make room.

## 1. Behaviour

- **When it happens:** while the pointer is over the dock's band (the
  dock shown, not sliding, no menu open), icons grow toward the pointer:
  - **up to 84 px**, from 52 at rest;
  - **along a bell curve**: (1 − t²)², where t is the distance from the
    icon's resting centre over 150 px.
- **How it moves:**
  - **Icons grow from their bottoms.** The dock's background stays the
    same height and widens to hold them.
  - **The item under the pointer stays under the pointer.** The row is
    laid out with each item's growth, then shifted so the point under the
    pointer does not move.
  - **Clicks are matched against the resting positions**, so a click on
    an icon picks the same app with or without magnification, including
    the window thumbnails after the divider.
- **Easing:** the amount eases in and out over 140 ms when the pointer
  arrives on or leaves the icons. The bottom-edge reveal zone, below the
  icons, does not magnify, so the dock can be shown without it.
- **Labels** sit above the grown icon. **Running dots** follow their
  icons.

## 2. Drawing

- **The icon cache is drawn at 84 px** (the largest size) and scaled down
  with bilinear filtering of premultiplied pixels (`bilerp`), so icons
  are sharp at every size. They are never scaled up.
- **Window thumbnails** after the divider use the same layout. They are
  scaled from their window layers, as before.
- **The redrawn area** reaches 32 px higher (the extra height) above the
  dock. It is redrawn while the amount changes and while the pointer
  moves over the dock.

## 3. Checks

- **`dock-magnify`:**
  - `1-magnified`: with the pointer on Crash Test's icon, its grey tile
    reaches 24 px above the dock's top edge: 84 − 52 = 32 px of growth,
    less the 8 px the icon rests below the top.
  - `2-settled`: with the pointer down at the very bottom edge, the tile
    is back below the edge, and the running dots are back at their resting
    places.
- **`dock`'s hover screenshots** now count the running dots and look for
  one under the pointer (where the hovered icon stays), because magnified
  neighbours move.
- **Negative control:** magnification switched off → `FAIL dock-magnify`
  (`reaches only 0 px above the dock`).

## 4. Not done

- **The launch bounce,** and a setting for the size (or turning it off).
- **Frame cost** of magnified dock frames was not measured separately.
  The dock is small (four scaled icons per frame).
