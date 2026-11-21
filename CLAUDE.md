# Gramcast harness rules

Built with the "Beyond vibe coding" workflow. The full build plan is the source of truth for scope.

## Always
- Quality and security of the code come ahead of prose. Be efficient.
- Red/green TDD: write a failing test, make it pass, then refactor.
- Commits of 400 lines or fewer. Methods under 100 lines.
- Linter on every edit, task tests on every task, all tests when a slice is done:
  `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo nextest run --workspace`.
- Logic lives in `crates/domain`. The Worker and API only validate and map.
- Platform, storage and scheduler code sit behind traits (ports and adapters).
- Show proof at each step: test summaries, and screenshots for UI.
- Every screen shows the BC Guides logo (brand assets in web/public/brand/).

## Never (rules, because these can't be taken back)
- Never commit secrets, tokens or `.dev.vars`. Secrets live in Worker secrets and GitHub secrets.
- Never log tokens, API keys or presigned URLs.
- Never post to a real Instagram or YouTube account from tests. Use fakes.
- Never add a dependency without checking it exists, is maintained and is the intended package.

## Agents (see .claude/agents)
- `developer` (Sonnet) writes all code, test first.
- `code-reviewer` (Opus) reviews every change. A different model from the developer, capped at three turns.
- `security-reviewer` (Opus) reviews every change with Cloudflare's security-audit skill (`.claude/skills/security-audit`) in guidance mode.
- `judge` (Opus, optional) scores a finished slice against the acceptance criteria.
- Any failed check goes back to the developer. After three review turns, stop and ask the owner before continuing.
