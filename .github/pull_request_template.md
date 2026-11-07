# External (non-OpenAI) Pull Request Requirements

External code contributions are by invitation only. Please read the dedicated "Contributing" markdown file for details:
https://github.com/openai/codex/blob/main/docs/contributing.md

If your PR conforms to our contribution guidelines, replace this text with a detailed and high quality description of your changes.

Include a link to a bug report or enhancement request.

## Checklist

- [ ] New `codex-core` integration tests (security tests especially) that run a
      host shell, rely on Unix paths or check sandbox behaviour start with
      `skip_if_wine_exec!(…)` when they can't pass against the Windows exec
      server. The Wine-exec suite runs only in nightly CI, so a missing skip
      turns nightly red after merge.
- [ ] Literal arguments (`None`, `true`, numbers) in new code, tests included,
      carry a `/*param*/` comment. PR CI lints the crates you changed.
- [ ] After changing a type (or a doc comment) in `codex-protocol` or
      `codex-app-server-protocol`, or a crate whose types they export
      (`codex-security-policy`, `codex-utils-absolute-path`,
      `codex-utils-path-uri`, `codex-extension-items`), run
      `just write-app-server-schema` and
      `just write-app-server-schema --experimental`, and commit the fixtures.
