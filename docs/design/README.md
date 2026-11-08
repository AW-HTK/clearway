# Clearway · visual reference

[Français](README.fr.md)

The three approved mockups are kept here alongside the implementation:

- [Application](App.dc.html): ticket strips, activity, resources and connected sessions.
- [Setup](Setup.dc.html): maintenance choices and version transition.
- [GitHub README](Main.dc.html): editorial hierarchy and product identity.

The originals came from `~/tmp/orchestrateur/clearway/maquette/`. These are design references, with illustrative data,
not screenshots of the running application. The exported canvas metadata is preserved in `canvas.json`.

## Native implementation

`src/visual.rs` owns the palette, bundled typography, panels and buttons used by both Windows executables. Inter
(regular and semibold) and JetBrains Mono are loaded privately from memory; they are not installed on the machine.
Segoe UI and Consolas are fallbacks if GDI cannot load the fonts. Font sources and licences are in `assets/fonts/`.

The window keeps the native Windows title bar and real controls for keyboard navigation and accessible text. Ticket
cards are owner-drawn list items: selection and scrolling are native, and the full selected ticket appears in a
selectable, scrollable detail area. Cancel, release and force actions retain their confirmations. Resource values,
alerts, activity and connected sessions come from the live server, with no mockup data in the application.

Setup keeps its existing discovery, installation, repair, uninstall and verification flows. The maintenance choices
use native radio buttons inside cards; the primary action is on the right. It does not claim that a signature has
been verified merely because the setup was opened.

## README rendering

The READMEs use GitHub-compatible HTML for the logo, text heading and navigation, followed by Markdown tables and
expandable technical documentation. No custom CSS, embedded page screenshot, or scripted widget is required.
The older SVG banners remain available as artwork, but are no longer used as the README header.

## Building a local installer

For a visual check without installation, `clearway-setup.exe --preview` opens the actual maintenance screen off
screen with sample version numbers. Install, repair, uninstall and verification actions are blocked in this mode,
which takes precedence over other command-line options. `tests/visual-preview.ps1 -Project <repo> -Output <folder>`
captures both native executables using fresh temporary state and a dedicated pipe, then closes its processes.

With the release toolchain on `PATH`, run `sh scripts/build-local.sh`. The resulting installer in
`dist/local/clearway-setup.exe` bundles both freshly compiled executables. It is a local build, without a release
signature; the existing signed files in `dist/` are preserved. The script does not run tests or install anything.

`CLEARWAY_BUNDLE_DIR` optionally points to a complete bundle directory instead of `dist/` when compiling the Windows
setup. It must contain the Windows and Linux executables, install/uninstall scripts, `VERSION` and `SHA256SUMS`.
This permits packaging a local build without overwriting signed release files. Normal releases still use
`scripts/build-release.sh` and the maintainer's signature.
