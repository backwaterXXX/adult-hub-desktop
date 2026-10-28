# Agent build notes

## Rust and Windows package builds

In some agent shells, `cargo build` is intercepted by a resource wrapper that
tries to write launch state to a user state directory. That path may be
read-only in the agent workspace, causing Cargo to fail before compilation
starts. For a direct build, resolve and invoke the active toolchain binaries
instead of the `cargo` command on `PATH`:

```sh
RUSTC_BIN="$(rustup which rustc)"
CARGO_BIN="$(rustup which cargo)"
RUSTC="$RUSTC_BIN" "$CARGO_BIN" build --manifest-path desktop/Cargo.toml --target x86_64-pc-windows-gnu --release
```

For the complete Windows EXE package, run `AH_DIRECT_BUILD=1
scripts/package-windows.sh`. This uses the active Rust toolchain directly and
avoids the optional Docker `dev/run.sh` build path. The packaging script
increments the patch version in `desktop/Cargo.toml` and `desktop/Cargo.lock`
automatically once per build run (with re-entrancy protection). Delivered
builds must NEVER pass `SKIP_VERSION_BUMP=1`, ensuring each delivered release
gets a distinct patch version. `SKIP_VERSION_BUMP=1` is reserved strictly for
intermediate compiler checks or CI where tags are pre-determined. Keep builds
in the foreground so their exit status and compiler output stay attached to
the agent session.

## Releasing versions (in-app auto-update channel)

The app checks for updates via GitHub Releases: it downloads
`adult-hub-update-manifest.json` (plus `.sig`) from the repository's
`releases/latest/download` URL, verifies the detached Ed25519 signature
against the public key embedded in `desktop/crates/app/src/updater.rs`, then
downloads the EXE and `libmpv-2.dll`, checks SHA-256 and size in a staging
file, and swaps them via a PowerShell helper that restarts the app.

To publish a release that clients can update to:

1. Land the version bump in `desktop/Cargo.toml` on `main` (the packaging
   script bumps the patch automatically; CI's release job refuses any tag
   whose version differs from `Cargo.toml`).
2. Tag and push: `git tag desktop-vX.Y.Z && git push origin desktop-vX.Y.Z`.
3. The `Desktop CI & Release` workflow builds Linux and Windows packages,
   generates and signs the manifest with the `RELEASE_SIGNING_KEY` repository
   secret (the private half of the embedded public key; the local copy is
   `dev/release-signing-key.pem`, git-ignored — set it with
   `gh secret set RELEASE_SIGNING_KEY < dev/release-signing-key.pem`), and
   creates the GitHub Release with all assets.

Never commit the signing key or ship a manifest the client cannot verify:
unsigned or mismatched-key releases are invisible to the updater.

## Prywatne testy interfejsu graficznego

- W przypadku Adult Hub i innych treści dla dorosłych nigdy nie wyświetlaj interfejsu ani treści na fizycznym ekranie HDMI, w sesji użytkownika, przez SSH X11 forwarding lub MobaXterm. Nie używaj odziedziczonego `DISPLAY`, istniejących serwerów X11 ani istniejących ekranów wirtualnych, w szczególności `:10`; nie zakładaj, że numer ekranu identyfikuje bezpieczną sesję.
- Gdy test GUI jest potrzebny, utwórz nową, prywatną instancję Xvfb wyłącznie dla tego testu, z automatycznie przydzielonym wolnym numerem ekranu (`-displayfd`), bez nasłuchu TCP (`-nolisten tcp`) i z osobnym plikiem autoryzacji X (`-auth`) w katalogu tymczasowym o uprawnieniach 0700. Utwórz losowy cookie przez `xauth`, nie wypisuj go ani nie ujawniaj zawartości pliku autoryzacji. Upewnij się, że aplikacja i narzędzia testowe używają wyłącznie jawnie ustawionych `DISPLAY` i `XAUTHORITY` tej instancji. Nie używaj `xhost +`, VNC ani innego udostępniania obrazu.
- Przed uruchomieniem aplikacji zweryfikuj połączenie z nową instancją i przerwij test, jeśli nie można potwierdzić izolacji. Nie używaj w zamian istniejącego ekranu ani HDMI. Jeśli sandbox uniemożliwia utworzenie gniazda X11, użyj właściwej procedury zgody na uruchomienie poza sandboxem; nie omijaj jej.
- Zrzuty i inne materiały z testów przechowuj tylko w prywatnym katalogu tymczasowym, nie pokazuj ich w rozmowie ani użytkownikowi, chyba że użytkownik wyraźnie o to poprosi. Po testach zakończ uruchomione przez siebie procesy i usuń katalog tymczasowy, plik autoryzacji oraz zrzuty. Nie przerywaj ani nie usuwaj zasobów innych sesji.
- Sam Xvfb służący testowi nie zajmuje kolejki ciężkich zadań przez cały czas działania; kompilacje, duże zestawy testów i inne ciężkie zadania nadal uruchamiaj przez `ai-build` zgodnie z poniższymi zasadami.
