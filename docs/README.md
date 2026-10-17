# Adult Hub Desktop — Planning Documents

Status: planning only. No application implementation has started.

These documents describe a functional Windows/Linux port of Adult Hub TV and Mobile. The selected foundation is Rust, Slint and libmpv. Android remains a Kotlin application.

## Reading order

| Document | Purpose |
| --- | --- |
| [General plan](general-plan.md) | Architecture, technology decisions, scope and release criteria. |
| [Stage 1 — Player prototype](stage-01-player-prototype.md) | Establish that embedded playback works on the supported platforms. |
| [Stage 2 — Authentication and browsing](stage-02-auth-and-browsing.md) | Implement API clients, local storage, authentication and content discovery. |
| [Stage 3 — Playback parity](stage-03-playback-parity.md) | Reproduce Android playback and progress behavior. |
| [Stage 4 — Desktop UI and supporting features](stage-04-desktop-ui-and-features.md) | Complete desktop interactions, settings, privacy and pHash coordination. |
| [Stage 5 — Packaging and release](stage-05-packaging-and-release.md) | Produce distributable packages, update support and release evidence. |

## Execution rules

- Implement stages in order. Stage 1 is a feasibility gate before full application development.
- Mark a stage complete only when its acceptance checklist and validation evidence are recorded.
- Use the Android implementation at revision `37deee2` in `/app/adult-hub-tv` as the initial behavior baseline. Record later baseline changes explicitly.
- Keep backend code, scripts and databases in `/app/adult-hub` read only. Automated tests use fixtures or isolated test doubles; production data is not a test target.
- Keep application UI, documentation and release notes in English.
- Fetch current official documentation through Context7 when implementing library-specific integrations. Pin selected dependency versions and commit the Cargo lockfile during implementation.
- Creating this documentation does not authorize starting implementation or publishing releases.

## Completion definition

The project is ready for its first release when all five stages pass, every baseline feature has an accepted desktop equivalent, and packaged applications pass the supported platform checks. Prototype playback alone is not a completed port.
