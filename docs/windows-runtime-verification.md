# Windows runtime verification

The current build environment is Linux and has neither Windows nor Wine, so it
cannot verify the native window, the mpv overlay, or Windows DPI scaling. The
Windows executable can be built here, but the runtime checks below still need
to be performed on a Windows machine before release.

## Launch

1. Extract the release bundle to a writable directory. Keep
   `adult-hub-desktop.exe`, `libmpv-2.dll`, and the other packaged runtime files
   together; do not launch the executable from inside the archive.
2. Start the executable and connect to a test Jellyfin/Resolver instance.
3. For thumbnail diagnostics, use the app's diagnostics log. Share only
   redacted entries containing the category, result class (for example
   `HTTP 403`, timeout, invalid response, decode failure), and a stable
   anonymized item label. Remove hostnames, paths, query values, tokens, and
   account details.

## Visual and interaction checks

At each Windows display scale (100%, 125%, and 150%, where available), check
the following client sizes: `960 × 640`, `1180 × 790`, and `1440 × 900`.

- Home: all first and last cards are fully visible; section titles align to the
  card grid; Retry affects only the selected thumbnail.
- Browse: hash-like IDs are not shown as titles; search, filters, sorting, and
  the final row remain usable without horizontal overflow.
- Channels: verify an accessible channel, an empty channel, no matches, offline
  service, and HTTP 403. Confirm technical details are tucked away and Retry
  does not clear the filters or move the scroll position.
- Player: with no media selected, confirm no active transport controls appear.
  With a neutral local video, check Play/Pause, seeking, keyboard focus,
  fullscreen, control visibility, and return to the window.
- Settings: confirm the connection badge stays compact, fields align to the
  left, and save/cancel behavior is unchanged.
- Navigate with keyboard only and confirm visible focus on all interactive
  controls.

Record screenshots for Home, Browse, Channels, empty Player, and Settings at
`1180 × 790` (100% scale), plus any failing layout at the scale and size where
it occurs. Mark each check as runtime-confirmed, test-confirmed, built but
unverified, or not completed; a successful build does not confirm runtime
behavior.
