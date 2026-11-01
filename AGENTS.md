# AGENTS.md

Operating rules for AI coding agents (Claude Code, Hermes, pi, Codex, …) working in this
repository. One job: **commit messages decide what gets released**, so choose them by
customer impact.

## How releases work here

`master` is released automatically by semantic-release
(`.github/workflows/release.yml` → `release.config.cjs`) on every push. It reads the
**commit messages that land on `master`** — not PR titles, not labels, not changed files.
Each conventional commit becomes a CHANGELOG entry and a version bump published to npm.

| commit type | release | use for |
|---|---|---|
| `feat` | minor | new capability a package consumer can use |
| `fix` | patch | bug fix a package consumer can notice |
| `perf` | patch | measurable speed / memory / payload improvement |
| `refactor` | patch | internal restructuring that changes observable behaviour |
| `docs` | patch | README and documentation |
| `chore` | patch | other internal work |
| any type with `!` or a `BREAKING CHANGE:` footer | major | breaking public API / ABI change |

## Rule: choose the type by customer impact, never by the file you touched

`feat`, `fix`, `perf` and `refactor` are **reserved for changes a user of the published
npm package can notice**: public API, upload behaviour, encryption output, reliability,
performance, documented defaults, platform support.

Internal-only work must not use them:

- `.github/workflows/**`, CI, Dependabot, lockfiles, repo tooling → `chore(ci):`
- README, docs, comments, typos → `docs(…)` or `chore(docs):`
- tests only, no production change → `test(…)`
- build scripts, `scripts/**`, codegen plumbing → `chore(build):`

A CI-only, docs-only or test-only change must never be `fix:` or `feat:` — it would cut a
release that contains nothing for consumers and still has to be supported. The reverse is
just as wrong: do not bury a user-visible change in `chore:`/`docs:`, it then ships
invisible in the release notes.

If a branch mixes internal and customer-facing work, use **one conventional commit per
concern** instead of stretching one type over both.

## Format

Conventional Commits: `type(scope): imperative summary` — lowercase, no trailing period,
no `[agent]` or other prefix in the commit itself. Scopes: `ios`, `android`, `web`,
`encryption`, `upload`, `ci`, `docs`, `e2e`, …

## Pull requests

- `master` takes **merge commits**, so branch commits are the release input. Keep each
  branch commit conventionally typed.
- Do not squash-merge an agent PR: GitHub then uses the PR title, and agent PR titles are
  prefixed with `[agent] ` by the workflow (`title-prefix` in
  `.github/workflows/agent-serviceuser.md`), which is not a conventional header — the
  release would silently skip the PR.
- Write PR titles conventionally anyway (`fix(encryption): …`); the branch commits are
  what ships, but the title is what reviewers and search read.

## Never edit release artefacts

Do not bump `version` in `package.json` / `example/package.json`, edit `CHANGELOG.md`, or
create tags by hand. semantic-release owns all three and its `chore(release): … [skip ci]`
commit is the only writer.

## Before committing

- `npm run typecheck` for TypeScript changes.
- The affected e2e/build target for native, Rust or WASM changes (see README → Contributing).
- Report exactly what you ran. Never claim a check passed that you did not run.
