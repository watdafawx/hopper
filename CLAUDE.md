@AGENTS.md

## Claude Code
- Load the matching skill before the work (in `.claude/skills/`): `hopper-gpui-testing` (run or check a desktop change on screen), `hopperd-scratch` (anything that starts Hopper Server or tests its web UI), `hopper-design` (any UI change), `hopper-crash-engine` (crash or stuck-start detection).
- Mocks are published as Design Artifacts (Artifact tool, `intent: "design"`), one artboard per option; give the link.
- Agents: Sonnet for mechanical or well-specified work, Opus for complex UI or logic. Split parallel agents by file ownership; they don't commit, run the app or format. Review their reports before calling anything done.
- Machine details: `CLAUDE.local.md` (untracked, loaded automatically).
