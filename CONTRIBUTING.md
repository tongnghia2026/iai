# Contributing to iAi

Thanks for your interest in iAi. Contributions of all kinds are welcome: bug
reports, fixes, new tools, filters, formats, UI improvements and documentation.

## License of contributions

iAi is released under the **GNU Affero General Public License v3.0 or later**
(see [LICENSE](LICENSE)). By submitting a contribution — a pull request, patch,
or any code, documentation or other material — you agree that:

1. Your contribution is licensed to the project and its users under
   `AGPL-3.0-or-later`, the same license as the rest of iAi
   ("inbound = outbound").
2. You also agree to the **Contributor License Agreement** in [CLA.md](CLA.md),
   which additionally lets the project maintainer license your contribution
   under other terms (for example, a paid commercial license). You keep the
   copyright to your own work; you are only granting these permissions.

This dual arrangement is what lets iAi stay fully open source **and** offer a
paid, closed commercial license to organizations that cannot use AGPL. Without
it, a single outside contribution would remove the maintainer's ability to do
so.

## Certifying your contribution (DCO)

Every commit must be signed off under the
[Developer Certificate of Origin](https://developercertificate.org/) — a short
statement that you wrote the change or otherwise have the right to submit it.
Add the sign-off automatically with:

```bash
git commit -s -m "your message"
```

which appends a line like:

```
Signed-off-by: Your Name <your.email@example.com>
```

By signing off you confirm the DCO **and** your acceptance of [CLA.md](CLA.md).

## How to contribute

1. **Bug report** — open a GitHub issue with reproduction steps, your OS/GPU,
   the complete status message, and a sample image you have permission to
   share.
2. **Small fix** — open a pull request directly.
3. **Larger change** (a new engine feature, a new format, an architectural
   change) — please open an issue to discuss it first, so the work is not
   wasted.

## Code style

- Rust code must pass `cargo fmt --check` (CI enforces this). Run `cargo fmt`
  before pushing.
- Keep the build green: `cargo test --lib` should pass.
- Comments in code are English and minimal. Do not change existing
  Vietnamese user-interface strings.
- Follow the conventions of the surrounding code (naming, module layout, and
  the established canvas/input/render paths).

## Commit messages

Use conventional-commit style where practical, e.g. `fix(canvas): ...`,
`feat(text): ...`, `docs(license): ...`.

Thank you for helping iAi.
