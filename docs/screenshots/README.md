# Screenshots

These images are referenced by `packaging/dev.sparkamp.Sparkamp.metainfo.xml`
and are what GNOME Software and Flathub display on the app's listing page.
They are served from `raw.githubusercontent.com` on the default branch, so a
file must be committed and pushed before the store can show it.

## Expected files

| File | Shows |
|---|---|
| `player.png` | The main player window, a track playing, visualizer running |
| `playlist.png` | The playlist window with a populated list |
| `media-library.png` | The Media Library, Files view, sidebar visible |
| `album-gallery.png` | The album gallery with cover art |
| `settings.png` | The Settings window, Appearance tab |

`player.png` is the `type="default"` screenshot — it is the one shown first.

## Capture conventions

- Use the built-in **Dark** skin, so the shots match what a new user sees.
- No personal metadata in frame: use music you are willing to publish, or
  rename tags first. These end up on a public store listing.
- Capture the window only, not the whole desktop, and include the window
  shadow if your compositor provides one.
- PNG, at 1x scale. Do not upscale.
- Keep each file well under 1 MB.
