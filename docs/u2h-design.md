# UI step 2h (U2h): the launch bounce

Status: implemented 2026-11-02 (version 0.71.0, with U4b). On a Mac, an
app's dock icon hops while the app opens. ZooOS now does the same.

## 1. Behaviour

- **When it starts:** when an application's first window opens because
  it was asked for: a dock click, Open in Files' Applications, or a
  `launch:` request. The windows the desktop opens at start-up do not
  bounce.
- **The hops:** two parabolic hops over 640 ms. The first rises 24 px;
  the second is smaller (40 % as high), as the icon settles. Hops add to
  any magnification.
- **The dock stays up** while an icon bounces, even with the pointer
  away.
- **Shift slows it down** ten times, as it does every animation.
- **When it ends,** the window server logs `animation bounce <app>:
  frames N time M ms`, like the window animations.

## 2. Check

- **`dock-bounce`:**
  1. Click Notes' dock icon with Shift held.
  2. Move the pointer off the icons, so magnification does not move the
     icon too.
  3. About 1.2 s in, the screenshot finds Notes' yellow band 14 px above
     the icon's resting top, where at rest there is only wallpaper.
  4. The bounce then ends, with at least 20 frames and 6 to 8 s in slow
     motion.
