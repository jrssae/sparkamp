# Screenshots

Two listings want screenshots, and they want different ones. Keep them apart.

## Flathub, and this directory

The five files below are referenced by name from
`packaging/dev.sparkamp.Sparkamp.metainfo.xml`, which is what GNOME Software
and Flathub read. They are served from `raw.githubusercontent.com` on the
default branch, so a file must be committed and pushed before either store can
show it. Renaming one breaks the listing silently.

| File | Shows |
|---|---|
| `player.png` | The main player window, a track playing, visualizer running |
| `playlist.png` | The playlist window with a populated list |
| `media-library.png` | The Media Library, Files view, sidebar visible |
| `album-gallery.png` | The album gallery with cover art |
| `settings.png` | The Settings window, Appearance tab |

`player.png` is the `type="default"` screenshot, so it is the one shown first.

## Mac App Store

A different set of up to ten, listed in `docs/store/app-store-listing.md`.
They are uploaded to App Store Connect rather than committed here, and they
have a hard size requirement the Flathub shots do not: every image in the set
must be the same size, and one of 1280x800, 1440x900, 2560x1600 or 2880x1800.

On a Retina display a 1280x800 logical region captures as 2560x1600 physical:

```
screencapture -x -R 80,50,1280,800 shot.png
```

Use that fixed region for every shot. Framing by hand drifts, and a set at
mixed sizes is rejected.

## Conventions, both sets

- Use a built-in skin, so the shots match what a new user sees. The Flathub
  five are Dark; the App Store set deliberately mixes Dark and Light to show
  that skins exist.
- No personal metadata in frame: use music you are willing to publish, or
  rename tags first. These end up on public store listings.
- Capture the window only, not the whole desktop, and include the window
  shadow if your compositor provides one.
- PNG, at 1x scale. Do not upscale.
- Keep each file well under 1 MB.
- The sandboxed macOS build keeps its library in its own container, which
  starts empty. Add a folder before shooting anything that needs one.
