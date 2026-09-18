# Release Process

## Versioning

Praxis uses [Semantic Versioning][semver]. The workspace
version is the single source of truth, defined in
`workspace.package.version` in the root `Cargo.toml`. All
workspace crates inherit this version.

[semver]: https://semver.org/

## Pre-release Checklist

Before tagging a release:

- [ ] Lints are clean (`make lint`)
- [ ] All tests pass locally (`make test && make test-integration && make test-conformance`)
- [ ] Dependency audit passes (`make audit`)
- [ ] SemVer compliance verified (`make semver`)
- [ ] Benchmarks have been run; performance is similar
  or better than the previous release
- [ ] Version in root `Cargo.toml` is bumped
  (both `workspace.package.version` and
  `workspace.dependencies` inter-crate versions)
- [ ] `Cargo.lock` is regenerated with the new version
- [ ] `make publish-dry-run` succeeds (add
  `--allow-dirty` when running against uncommitted
  changes)
- [ ] `SECURITY.md` lists the new minor version
- [ ] GitHub Release changelog is drafted (see below)

## Tagging a Release

Tags follow the format `v<MAJOR>.<MINOR>.<PATCH>` (e.g.
`v0.1.0`) and must match `workspace.package.version`;
the release workflow rejects mismatched tags. Push the
tag to the repository:

```console
git tag v0.1.0
git push origin v0.1.0
```

The release runs in two phases
(`.github/workflows/release.yaml`).

Phase 1 runs on the tag push:

1. Validate the tag against the workspace version
2. Run the full test suite (skipped when the commit is
   already green on main)
3. Verify every release crate packages cleanly
   (publish dry run)
4. Build and publish the container image to GHCR
5. Cut a draft pre-release with generated notes

Phase 2 runs when a maintainer publishes the draft:

6. Publish every release crate to crates.io, in
   dependency order

Review and edit the draft notes, then publish the
release from the GitHub UI. Publishing performs the real
crates.io publish using the `RUST_CRATES_PUBLISH_TOKEN`
secret; nothing reaches crates.io until you publish the
release.

## Publishing Container Images

Container images are published to [GitHub Container
Registry][ghcr] (GHCR) by the release pipeline. Outside
of a release, the **Publish** workflow
(`.github/workflows/publish.yaml`) can be run manually
via `workflow_dispatch`. It now actually builds and
publishes the image (it was previously a no-op, gated
behind a job condition that never held on a manual
run). Use GitHub's "Run workflow" ref selector to pick
the branch or tag to build from; the workflow publishes
whichever ref you dispatch it against (a branch dispatch
tags the image with the branch name and `:sha-<hash>`, a
tag dispatch with only `:sha-<hash>`).

[ghcr]: https://ghcr.io/praxis-proxy/praxis

### Image Tags

Two workflows push image tags, and they do not push the
same set. Both use inline, SHA-pinned steps, so the tag
set and timing are controlled here rather than by an
external action. The release workflow (`release.yaml`)
pushes the tags below across Phase 1 and Phase 2. The
manual **Publish** workflow (`publish.yaml`) pushes only
ref-identifying tags: the immutable `:sha-<hash>` plus
the branch name. It never advances the moving `:latest`
or `:<major>.<minor>` tags, so a manual run cannot
repoint consumers.

| Pattern | Example | Pushed |
| --------- | --------- | ------------- |
| `sha-<hash>` | `sha-abc1234` | Phase 1 releases and manual Publish runs |
| `<version>` | `0.1.0` | Phase 1, every release |
| `<major>.<minor>` | `0.1` | Phase 2, stable releases only |
| `latest` | `latest` | Phase 2, stable releases only |
| `<branch>` | `main` | Manual Publish runs only |

Phase 1 publishes only the immutable `:<version>` and
`:sha-<hash>` tags. The moving `:<major>.<minor>` and
`:latest` tags are advanced in Phase 2, and only after a
stable (non pre-release) release is published, so they
never point at a pre-release build. A pre-release
therefore gets only `:<version>` and `:sha-<hash>`.

## Changelog

Praxis uses [GitHub Releases][gh-releases] for
changelogs. Each release is created through the GitHub
UI after pushing a tag. Use GitHub's "Generate release
notes" feature to auto-populate from merged PRs, then
edit for clarity. There is no separate CHANGELOG file.

[gh-releases]: https://github.com/praxis-proxy/praxis/releases

## Release Branches

Release branches are optional and created from tags when
backports are needed. The naming convention is
`release/v<MAJOR>.<MINOR>.x` (e.g. `release/v0.1.x`).

Fixes are cherry-picked onto the release branch, a new
patch tag is created from it, and the release workflow
runs as usual (the tag push drives `release.yaml`).

## Container Details

The production image is a minimal Alpine container:

- Static musl build with LTO, single codegen unit, and stripped symbols
- Runs as non-root user (`praxis`)
- Exposes ports `8080` (proxy) and `9901` (admin)
- Built-in health check at `http://127.0.0.1:9901/healthy`
- Config directory: `/etc/praxis`

> **Note**: This is subject to change.
