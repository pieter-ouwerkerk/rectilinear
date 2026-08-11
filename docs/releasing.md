# Publishing a release

Rectilinear has two crates that are released at the same version:

- `rectilinear-core`, the library crate
- `rectilinear`, the CLI crate, which depends on `rectilinear-core`

Publishing is currently manual. Publish `rectilinear-core` first, wait for it to
become available in the crates.io index, and then publish `rectilinear`.
Published crate versions cannot be replaced, so inspect and verify the packages
before uploading them.

All `cargo publish` commands, including dry runs, must run from a clean release
commit. Do not use `--allow-dirty` to bypass Cargo's check: commit and merge the
version bump first so the reviewed commit is exactly what gets packaged.

## 1. Prepare the release

Start from a clean branch based on the latest `main`. Choose the new version and
previous release tag for the commands below:

```sh
release_version=X.Y.Z
previous_tag=vA.B.C
```

Update these three values:

1. The root package version in `Cargo.toml`
2. The `rectilinear-core` dependency version in the root `Cargo.toml`
3. The package version in `crates/rectilinear-core/Cargo.toml`

Refresh the workspace lockfile and confirm that both workspace packages have the
new version:

```sh
cargo check --workspace
git diff -- Cargo.toml crates/rectilinear-core/Cargo.toml Cargo.lock
cargo metadata --no-deps --format-version 1
```

## 2. Validate the release

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
```

## 3. Commit and merge the version bump

```sh
git add Cargo.toml crates/rectilinear-core/Cargo.toml Cargo.lock
git commit -m "Release v${release_version}"
```

Push the change through the normal pull request flow. After it is merged, update
the local `main` checkout and ensure it is clean:

```sh
git switch main
git pull --ff-only
git status --short --branch
```

All remaining commands must run from this exact release commit.

## 4. Validate the release packages

From the clean release commit, inspect exactly what will be uploaded. In
particular, check for local settings, credentials, large fixtures, and
internal-only files:

```sh
cargo package -p rectilinear-core --list
cargo package -p rectilinear --list
cargo publish -p rectilinear-core --dry-run --locked
```

The CLI's full dry run may not resolve its registry dependency until the matching
`rectilinear-core` version has been published. The workspace tests still validate
the CLI against the local core crate before anything is uploaded.

## 5. Publish to crates.io

Authenticate with `cargo login` first if this machine does not already have a
crates.io token.

Publish the core crate:

```sh
cargo publish -p rectilinear-core --locked
```

Wait until crates.io exposes the version. A successful upload can take a short
time to appear in the index:

```sh
cargo info "rectilinear-core@${release_version}"
```

Once that succeeds, validate and publish the CLI:

```sh
cargo publish -p rectilinear --dry-run --locked
cargo publish -p rectilinear --locked
```

If `cargo publish` times out while waiting for the index, check crates.io with
`cargo info` before retrying; the upload may already have succeeded.

## 6. Verify the published crates

```sh
cargo info "rectilinear-core@${release_version}"
cargo info "rectilinear@${release_version}"
```

## 7. Tag the release

Create an annotated tag on the exact commit used for publishing and push it:

```sh
git tag -a "v${release_version}" -m "Rectilinear ${release_version}"
git push origin "refs/tags/v${release_version}"
```

Do not let GitHub create an implicit tag: explicitly pushing the annotated tag
ensures the release points to the reviewed and published commit.

## 8. Create the GitHub release

```sh
gh release create "v${release_version}" \
  --verify-tag \
  --title "Rectilinear ${release_version}" \
  --generate-notes \
  --notes-start-tag "${previous_tag}" \
  --fail-on-no-commits
```

Finally, verify the release and remote tag:

```sh
gh release view "v${release_version}"
git ls-remote --tags origin "v${release_version}"
```

## 9. Optional: update the local CLI

Install the published version through Cargo so the local `rectilinear` command
points to the new release:

```sh
cargo install rectilinear --version "${release_version}" --locked --force
rectilinear --version
```

Useful references:

- [Publishing on crates.io](https://doc.rust-lang.org/cargo/reference/publishing.html)
- [`cargo publish`](https://doc.rust-lang.org/cargo/commands/cargo-publish.html)
- [`gh release create`](https://cli.github.com/manual/gh_release_create)
