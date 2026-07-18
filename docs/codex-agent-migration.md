# Claude Code to Codex Migration

This repository now uses Codex-native agent guidance. The authoritative project
instructions are in [`AGENTS.md`](../AGENTS.md).

## What Changed

| Previous Claude surface | Codex replacement | Migration decision |
| --- | --- | --- |
| Root `CLAUDE.md` | Root `AGENTS.md` | Project facts, commands, constraints, verification, and documentation rules migrated. |
| `.claude/settings.local.json` | Codex permission/sandbox policy | Removed rather than copied. The Claude tool allowlist has no safe one-to-one project setting. |
| `/browse` and Claude-in-Chrome rules | `browser:control-in-app-browser` | Use Codex's browser skill for local UI inspection and testing. |
| gstack planning commands | Codex Plan mode | Use Plan mode for ambiguous or high-risk work. |
| gstack review/QA commands | Codex review plus repository tests | Follow the verification matrix in `AGENTS.md`. |
| gstack reusable workflows | `.agents/skills/<name>/SKILL.md` | Rewrite only workflows the project repeatedly needs. Do not copy generated gstack files. |
| `.claude/worktrees/*` | Normal Git/Codex worktrees | Removed as generated legacy state, not retained as project source. |

## Why gstack Was Not Copied

The former `.claude/skills/gstack/` tree was a vendored tool distribution rather
than project guidance. It includes Claude hooks, `claude -p` execution,
Anthropic SDK/evaluation support, telemetry, compiled browser binaries,
generated Codex variants, dependency trees, and its own contributor
documentation.

Copying that directory into `.agents/skills` would preserve hidden Claude
runtime dependencies and add thousands of generated files. Codex already
provides planning, review, browser control, subagents, and reusable skills.
Project-specific workflows should be recreated as small skills only when a
repeated need is demonstrated.

## Configuration Boundaries

- Repository behavior and commands: `AGENTS.md`.
- Optional team-shared Codex settings: `.codex/config.toml`.
- Personal model, reasoning, approvals, sandbox, and MCP settings:
  `~/.codex/config.toml`.
- Reusable repository skills: `.agents/skills/<skill-name>/SKILL.md`.
- Runtime logs, sessions, worktrees, and local credentials: never commit.

No project `.codex/config.toml` is required for this migration. The legacy
Claude allowlist only granted Claude-specific tools and should not be translated
into broader repository permissions.

## Working With Codex

1. Open Codex at the repository root so it discovers `AGENTS.md`.
2. Trust the repository only after reviewing its project configuration.
3. Ask Codex to use the Makefile commands from `AGENTS.md`.
4. Use Plan mode before complex or ambiguous changes.
5. Use the in-app browser for Kong Manager flows and real local UI validation.
6. Put new repeatable agent workflows in `.agents/skills`, following the Agent
   Skills format.

## GPT-5.6-Sol Prompting Posture

The root `AGENTS.md` is intentionally model-aware but not model-pinned. Model
selection and reasoning effort remain personal or task-level settings rather
than committed project defaults. This avoids forcing a flagship model onto
latency-sensitive work and keeps the repository usable across Codex surfaces.

Project guidance follows the GPT-5.6 prompting recommendations:

- instructions are stated once and organized around repository facts,
  constraints, authorization boundaries, verification, and completion;
- change requests authorize safe in-scope edits and non-destructive checks,
  while review and diagnosis requests remain read-only;
- prompts should specify outcomes, evidence, success criteria, and output
  contracts instead of asking the model to “think harder”;
- prompt and model migrations must preserve endpoint, tool, reasoning, caching,
  multimodal, latency, and cost behavior until representative tests justify a
  deliberate change;
- optional GPT-5.6 features are adopted independently, not bundled into a model
  string upgrade.

For AI Gateway examples, older model names may remain intentionally as
compatibility demonstrations, fixtures, tokenizer mappings, or routing cases.
Do not bulk-replace them with `gpt-5.6-sol`.

## Retired Legacy Tooling

The repository-local `.spec-workflow/`, `.claude/`, and `.gstack/` trees were
removed after their relevant guidance was migrated. They are no longer treated
as active tooling, compatibility surfaces, or ignored local state.

`.codex/` and `.agents/` are deliberately not ignored because they may contain
reviewable team configuration and repository skills.

## Intentionally Retained Claude References

Kong-Rust supports Anthropic and Claude models as AI Gateway products. Provider
drivers, codecs, tests, routes, and user documentation that mention Claude model
names or the Anthropic protocol are not Claude Code configuration and remain
valid.
