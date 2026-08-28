#!/bin/sh
set -eu

ci_workflow=.github/workflows/ci.yml
release_workflow=.github/workflows/release.yml

fail() {
  echo "release policy check failed: $*" >&2
  exit 1
}

# Reviewed Rust commands consume the workspace lock file.
if grep -En '(^[[:space:]]*cargo|run:[[:space:]]+cargo)[[:space:]]+(clippy|test|check|build|metadata)([[:space:]]|$)' \
  "$ci_workflow" "$release_workflow" |
  grep -Ev 'cargo[[:space:]]+(clippy|test|check|build|metadata)[[:space:]]+--locked([[:space:]]|$)'
then
  fail 'CI and release Cargo commands must use --locked'
fi

# Normal actions use immutable commits. During the coordinated v2 migration only,
# the Builder workflow may carry one conspicuous sentinel which must be replaced
# by its reviewed 40-hex commit before release.
if grep -En 'uses:[[:space:]]+[^.[:space:]]' "$ci_workflow" "$release_workflow" |
  grep -Ev 'uses:[[:space:]]+[A-Za-z0-9_.-]+/[A-Za-z0-9_./-]+@[0-9a-f]{40}([[:space:]]+#.*)?$|uses:[[:space:]]+isarmg/union-builder/\.github/workflows/build-union\.yml@UNION_BUILDER_V2_COMMIT_SHA_TO_PIN$'
then
  fail 'external actions must use immutable commits (or the one reviewed Builder v2 pin sentinel)'
fi

if grep -En '^[[:space:]]+image:[[:space:]]+' "$ci_workflow" "$release_workflow" |
  grep -Ev '@sha256:[0-9a-f]{64}([[:space:]]+#.*)?$'
then
  fail 'workflow service images must use immutable sha256 digests'
fi

if grep -En '(runs-on:|^[[:space:]]+os:[[:space:]]+\[).*-[l]atest' "$ci_workflow" "$release_workflow"
then
  fail 'workflow runners must not use mutable *-latest labels'
fi
if grep -En 'runs-on:[[:space:]]+' "$ci_workflow" "$release_workflow" |
  grep -Ev 'runs-on:[[:space:]]+(ubuntu-24\.04|windows-2025|macos-26|\$\{\{ matrix\.os \}\})$'
then
  fail 'workflow runners must use reviewed concrete labels'
fi

rust_action_count=$(grep -hF 'uses: dtolnay/rust-toolchain@' "$ci_workflow" "$release_workflow" | wc -l | tr -d ' ')
rust_version_count=$(grep -hF 'toolchain: 1.98.0' "$ci_workflow" "$release_workflow" | wc -l | tr -d ' ')
[ "$rust_action_count" -gt 0 ] && [ "$rust_version_count" = "$rust_action_count" ] ||
  fail 'every Rust toolchain action must select Rust 1.98.0'

grep -Eq '^  workflow_call:[[:space:]]*$' "$ci_workflow" ||
  fail 'CI must be callable from release'
grep -Fq '  group: release-${{ github.ref }}' "$release_workflow" &&
  grep -Fq '  cancel-in-progress: false' "$release_workflow" ||
  fail 'release runs for one ref must be serialized without cancellation'
grep -Fq '    uses: ./.github/workflows/ci.yml' "$release_workflow" ||
  fail 'release must reuse repository CI'
grep -Fq '    needs: verify-release-ref' "$release_workflow" ||
  fail 'repository CI must follow release-ref verification'
grep -Fq '    needs: [verify-release-ref, full-ci]' "$release_workflow" ||
  fail 'Builder must run only after source verification and repository CI'
builder_uses=$(grep -E '^[[:space:]]+uses: isarmg/union-builder/\.github/workflows/build-union\.yml@' "$release_workflow" || true)
[ "$(printf '%s\n' "$builder_uses" | grep -c .)" -eq 1 ] ||
  fail 'release must call exactly one Union Builder workflow'
builder_ref=${builder_uses##*@}
if [ "$builder_ref" != UNION_BUILDER_V2_COMMIT_SHA_TO_PIN ] &&
  ! printf '%s\n' "$builder_ref" | grep -Eq '^[0-9a-f]{40}$'
then
  fail 'release must pin the Builder v2 workflow to a reviewed commit'
fi
[ "$(grep -Fxc "      builder-revision: $builder_ref" "$release_workflow")" -eq 1 ] ||
  fail 'builder-revision must exactly equal the immutable Builder workflow uses ref'
[ "$(grep -Fxc '      materialize-caller-source: true' "$release_workflow")" -eq 1 ] ||
  fail 'the Union release must enable Builder caller-source materialization exactly once'
[ "$(grep -Fxc '      caller-revision: ${{ github.sha }}' "$release_workflow")" -eq 1 ] ||
  fail 'caller-revision must be the exact Union workflow github.sha'
grep -Fq '# Builder v2 owns schema-v2 composition.' "$release_workflow" ||
  fail 'release must document that Builder v2 owns schema-v2 inclusion'
grep -Fq '      profile: full' "$release_workflow" ||
  fail 'release must select the Builder official full profile'

for required in \
  'name: schema-v2 composition boundary' \
  'Assert Core has no business-module Cargo features'
do
  grep -Fq "$required" "$ci_workflow" ||
    fail "CI is missing required composition coverage: $required"
done
if grep -En '(features:[[:space:]]*module-|--features[^[:space:]]*[[:space:]]+module-)' \
  "$ci_workflow" "$release_workflow" server/Cargo.toml; then
  fail 'Cargo feature selection must not encode the schema-v2 module graph'
fi
if grep -Eiq 'server/packaging|n[Ff]PM' "$ci_workflow"; then
  fail 'legacy standalone Server packaging must not run in formal CI'
fi

grep -Fq 'if [[ ! "$GITHUB_REF_NAME" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then' "$release_workflow" ||
  fail 'release tags must be strict vMAJOR.MINOR.PATCH'
grep -Fq 'if [[ "$GITHUB_REF_NAME" != "v$workspace_version" ]]; then' "$release_workflow" ||
  fail 'release tag must match the workspace Cargo version'
grep -Fq 'git merge-base --is-ancestor "$release_commit" "$main_commit"' "$release_workflow" ||
  fail 'release tag must belong to main history'
grep -Fq "manifest_version=\"\$(jq -er '.distribution.version' \"\$manifest\")\"" "$release_workflow" ||
  fail 'release must read the version from union-release.json'
grep -Fq 'if [[ "$manifest_version" != "$version" ]]; then' "$release_workflow" ||
  fail 'manifest version must match the tag'
grep -Fq 'if [[ "$manifest_revision" != "$release_commit" ]]; then' "$release_workflow" ||
  fail 'manifest revision must match the tag commit'
grep -Fq '.schema_version == 2 and' "$release_workflow" ||
  fail 'release must require the Builder v2 release-manifest schema'
grep -Fq '["dufs", "host-monitoring", "photo-backup", "sentinel-monitor", "sunshine"]' "$release_workflow" ||
  fail 'the official full distribution must contain exactly five workers'
grep -Fq ".modules[] | [.id, .package, .manifest] | @tsv" "$release_workflow" &&
  grep -Fq ".execution.executable" "$release_workflow" &&
  grep -Fq 'test -f "$executable_path"' "$release_workflow" &&
  grep -Fq 'test -x "$executable_path"' "$release_workflow" ||
  fail 'release must resolve every module executable through its package manifest'
if grep -Fq ".modules[].executable" "$release_workflow"; then
  fail 'schema v2 modules do not carry executable paths in union-release.json'
fi
grep -Fq 'sha256sum --check SHA256SUMS' "$release_workflow" ||
  fail 'Builder distribution checksums must be verified'
grep -Fq 'union-distribution.tar' "$release_workflow" &&
  grep -Fq 'test -x "$distribution/bin/unionc"' "$release_workflow" ||
  fail 'the release must preserve and verify executable modes across artifact transport'
grep -Fq 'bash .github/scripts/create-reproducible-tar.sh' "$release_workflow" ||
  fail 'the Union distribution must use the reproducible archive helper'
grep -Fq 'sha256sum "$archive_dir.tar.gz" > SHA256SUMS' "$release_workflow" ||
  fail 'the published archive must have an outer SHA256SUMS'
[ "$(grep -c 'gh release create' "$release_workflow")" -eq 1 ] ||
  fail 'release must create exactly one GitHub Release'

for forbidden in server-linux unionc-agent nFPM nfpm '\.deb' '\.rpm' '\.msi' '\.pkg'; do
  if grep -Eiq "$forbidden" "$release_workflow"; then
    fail "release must not publish a standalone Server, worker, or companion artifact: $forbidden"
  fi
done

bash .github/scripts/test-reproducible-tar.sh
