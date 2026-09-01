# AGENTS.md — the snaraj/theme contract

This file is the canonical contract for agent work in this repository. The
shared skill (`agentic-skills/github-collaboration`) defers to it; where the
two disagree, this file wins. Two rules outrank everything: never spend
money, never trade away security.

## Authority

- **The owner alone merges.** Agents never push main, never force-push,
  never delete remote refs.
- Agents flip a PR to Ready only after an exact-head APPROVE receipt with
  green CI (ready-flip delegation, 2026-09-01 — own PRs included).
- Reviewers post **only** through the `snaraj-agent-reviews[bot]` App token;
  authors push with the owner keyring; read-only work uses the RO PAT.

## Branches and PRs

- Branch grammar: `<lane>-<effort>/<issue#>-<topic>`. Issues first; one
  writer per branch; one branch per worktree under `.claude/worktrees/`.
- Draft PRs only, until author-complete (final head, final body, evidence
  complete, CI green) — then apply `requires-review`.
- PR bodies carry `+/−` line accounting and end with the acting lane's
  signature (`- Fable5`, `- Opus5`, …) matching the lane label. No
  Co-Authored-By trailers, ever.
- Low code volume: deletion outranks addition; every diff is judged first on
  how little it adds.

## Review

- The receipt is a **normal issue comment**, never a `gh pr review` object.
  First line: `VERDICT: APPROVE head=<sha>` or
  `VERDICT: REQUEST-CHANGES head=<sha>`. The reviewer removes
  `requires-review` whichever way the verdict went.
- Any head change invalidates every receipt at the prior head.
- `cybersecurity-review-requested` routes to the security fleet and is
  removed only by that verdict.

## Commits

- Identity: `Samuel Naranjo <39077795+snaraj@users.noreply.github.com>`,
  SSH-signed per command with the registered signing key; GitHub must report
  `verified=true, reason=valid` on every commit before Ready.

## CI and releases

- Zero spend: public-repo GitHub-hosted runners only; every job earns its
  wall-clock; actions SHA-pinned with truthful version comments; workflow
  token stays read-only.
- Releases are immutable `v*.*.*` tags cut from main (ruleset-enforced: no
  update, no delete, no force-push). One release slot: parallel Drafts each
  claim base+1; declared merge order resolves who re-cuts.

## Code rules

- Rust stable, `cargo fmt --all --check`, `clippy -D warnings`, tests green —
  the same gate CI runs, executed locally before every push.
- `#![deny(unsafe_code)]` in every crate; exceptions need an owner ruling.
- The `pigment` crate emits strings and files only — it never touches a
  terminal, socket, or the desktop. Only the CLI applies state, and only
  through the documented paths (kitty socket `set-colors`, wallpaper tool,
  OSC writes to the caller's own tty).
- No credentials in the repo, in test fixtures, or on command lines
  (stdin-config pattern for anything secret).
