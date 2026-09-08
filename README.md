# artefacto

Interactive artifacts between you and your coding agent.

An agent emits structured data. artefacto renders it as an interactive page in
your browser. You comment, answer questions, tick steps, and approve. Every
interaction flows back to the agent as data, live, while the review is still
open. The first artifact kind is a development plan. Specs, change recaps,
checklists, and findings triage are planned to follow on the same spine.

artefacto is agent-agnostic. It ships as a single binary with a CLI that any
coding agent can drive, and a skill that teaches the loop. It is the
interactive-artifact plugin for [loadout](https://loadout.tools), and it also
runs on its own.

## Status

Early. The static renderer works: `artefacto plan check`, `render`, and
`status` validate an `artefacto.plan/1` document and produce a self-contained
HTML page. The interactive server, the page rewrite, and the artifact index
are not built yet.

The design spec is in `docs/superpowers/specs/`, and the implementation plans
are in `docs/superpowers/plans/`.

## License

MIT. See [LICENSE](LICENSE).
