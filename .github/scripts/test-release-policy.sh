#!/bin/sh
set -eu

ci_workflow=.github/workflows/ci.yml
release_workflow=.github/workflows/release.yml

fail() {
  echo "release policy check failed: $*" >&2
  exit 1
}

cargo_command_files="
$ci_workflow
$release_workflow
agent/packaging/linux/build-packages.sh
server/packaging/linux/build-packages.sh
"

# A release must consume the reviewed lock file. Match only command positions
# (including shell substitutions) so diagnostic strings such as
# "cargo metadata failed" are not mistaken for invocations.
if grep -En '(^[[:space:]]*cargo|run:[[:space:]]+cargo|=[[:space:]]*cargo|\$\(cargo)[[:space:]]+(clippy|test|check|build|metadata|pkgid)([[:space:]]|$)' \
  $cargo_command_files |
  grep -Ev 'cargo[[:space:]]+(clippy|test|check|build|metadata|pkgid)[[:space:]]+--locked([[:space:]]|$)'
then
  fail 'CI, release, and packaging Cargo commands must use --locked'
fi

job_has_line() {
  job_name=$1
  expected=$2
  awk -v header="  $job_name:" -v expected="$expected" '
    $0 == header { inside = 1; next }
    inside && /^  [A-Za-z0-9_-]+:$/ { exit }
    inside && $0 == expected { found = 1 }
    END { exit(found ? 0 : 1) }
  ' "$release_workflow"
}

grep -Eq '^  workflow_call:[[:space:]]*$' "$ci_workflow" ||
  fail 'CI is not callable from the release workflow'
awk '
  $0 == "concurrency:" {
    getline
    group = ($0 == "  group: release-${{ github.ref }}")
    getline
    cancel = ($0 == "  cancel-in-progress: false")
  }
  END { exit(group && cancel ? 0 : 1) }
' "$release_workflow" ||
  fail 'release runs for the same ref are not serialized without cancellation'
job_has_line full-ci '    uses: ./.github/workflows/ci.yml' ||
  fail 'release does not invoke the repository CI workflow'
job_has_line full-ci '    needs: verify-release-ref' ||
  fail 'full CI can start before release-source verification'
grep -Fq 'if [[ ! "$GITHUB_REF_NAME" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then' \
  "$release_workflow" || fail 'release tags are not restricted to strict semantic versions'
grep -Fq 'git fetch --no-tags origin' "$release_workflow" ||
  fail 'release-source verification does not refresh origin/main'
grep -Fq 'release_commit="$(git rev-parse "${GITHUB_SHA}^{commit}")"' "$release_workflow" ||
  fail 'release-source verification does not peel annotated tags to a commit'
grep -Fq 'git merge-base --is-ancestor "$release_commit" "$main_commit"' "$release_workflow" ||
  fail 'release-source verification does not require main ancestry'

for artifact_job in server-linux linux windows macos; do
  job_has_line "$artifact_job" '    needs: [verify-release-ref, full-ci]' ||
    fail "$artifact_job does not require release-source verification and full CI"
done

grep -Fq '          prerelease: true' "$release_workflow" ||
  fail 'unsigned tag releases must be marked as GitHub prereleases'
grep -Fq 'UnionC-Agent-{0}-x64-unsigned.msi' "$release_workflow" ||
  fail 'Windows release artifact is not explicitly named as unsigned'
grep -Fq 'unionc-agent-$version-unsigned.pkg' "$release_workflow" ||
  fail 'macOS release artifact is not explicitly named as unsigned'
grep -Fq 'This prerelease intentionally contains unsigned artifacts.' "$release_workflow" ||
  fail 'unsigned release warning is missing'
grep -Fq 'xargs -0 sha256sum > SHA256SUMS' "$release_workflow" ||
  fail 'unsigned release does not generate a checksum manifest'
grep -Fq 'source_date_epoch="$(git show -s --format=%ct "${GITHUB_SHA}^{commit}")"' \
  "$release_workflow" ||
  fail 'portable Linux archive timestamp is not derived from the release commit'
grep -Fq 'bash .github/scripts/create-reproducible-tar.sh' "$release_workflow" ||
  fail 'portable Linux archive does not use the reproducible archive helper'

bash .github/scripts/test-reproducible-tar.sh

for forbidden in \
  Set-AuthenticodeSignature \
  notarytool \
  attest-build-provenance \
  LINUX_SIGNING_KEY_BASE64 \
  WINDOWS_SIGNING_PFX_BASE64 \
  MACOS_SIGNING_P12_BASE64 \
  APPLE_NOTARY_KEY_P8_BASE64
do
  if grep -Fq "$forbidden" "$release_workflow"; then
    fail "release workflow must not execute signing, notarization, or attestation: $forbidden"
  fi
done
