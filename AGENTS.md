# Soberized project instructions

@/home/nino/.codex/RTK.md

## Git workflow

- Create a local commit after each coherent, successful change once its relevant
  available checks pass. Do this automatically without asking for confirmation.
- Use Conventional Commits: `type(scope): short imperative description`. The
  scope is optional. Choose an appropriate type such as `feat`, `fix`, `refactor`,
  `test`, `docs`, `build`, `ci`, `perf`, or `chore`.
- Keep commits focused. Stage only the files belonging to the completed change.
- Record relevant validation and any blocked host-only checks in the commit body.
  Do not describe skipped or unavailable checks as passing.
- Keep credentials, machine-local configuration, and build artifacts out of Git.
- Push commits only when the user requests it.
