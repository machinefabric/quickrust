# How to publish a QuickRust release

This guide shows you, as a maintainer of QuickRust, how to build and publish a release that the install scripts and `qrust self update` will find.

## Tag the commit

A release is made from a commit on `main` whose CI run passed. Its version is the one in `version.txt` at that commit (the version is set for every commit; do not edit it by hand). Tag the commit with that version, prefixed with `v`, and push the tag:

```bash
git checkout <commit>
git tag "v$(tr -d '[:space:]' < version.txt)"
git push origin "v$(tr -d '[:space:]' < version.txt)"
```

The tag starts the release workflow (`.github/workflows/release.yml`). It stops at once if the tag is not exactly `v` followed by `version.txt`'s content.

## Check the draft

The workflow builds QuickRust for x86_64 and aarch64 Linux (static, musl), x86_64 and aarch64 macOS and x86_64 Windows, checks that each binary reports the release's version, and creates a draft release with:

- `qrust-<target>.tar.gz` for each platform (`qrust` and `qrust-rustc-wrapper`) and its `.sha256`;
- `version.txt`, `install.sh` and `install.ps1`.

If a platform's build fails, the workflow fails and no draft is created: fix the cause on `main` and tag the new commit.

Open the draft on GitHub (`gh release view v<version>`) and check that every archive is there.

## Publish it

Publishing is deliberate and done by hand, because a published release reaches every installation: the install scripts and `qrust self update` read the latest published release.

```bash
gh release edit "v<version>" --draft=false
```

Then check it from a machine with QuickRust installed:

```bash
qrust self update
qrust --version
```

## If a release is broken

Mark it as a draft again (`gh release edit "v<version>" --draft=true`) so that new installs and updates fall back to the previous published release, fix the problem on `main`, and publish a new release. Installations that already updated stay on the broken one until the next release reaches them through `qrust self update`.

## Related

- [How to install, update and uninstall QuickRust](install.md)
- [Platforms](../reference/platforms.md) for what each archive is tested on
