# Assets

## Icon

`icon.svg` is the application icon: a timesheet (document with ruled rows and a header). Use it for app bundles, installers, or documentation.

To generate PNGs or macOS `.icns`:

- **macOS:** Open `icon.svg` in Preview, then File → Export → PNG. For `.icns`, use [iconutil](https://developer.apple.com/library/archive/documentation/GraphicsAnimation/Conceptual/HighResolutionOSX/Optimizing/Optimizing.html) with an `.iconset` folder of PNGs at 16, 32, 128, 256, 512, and 1024 px (and 2x variants if desired).
- **Web:** Use an SVG-to-PNG converter (e.g. cloudconvert.com) at 1024×1024 for a master PNG.
