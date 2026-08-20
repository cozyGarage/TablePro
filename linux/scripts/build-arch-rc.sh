#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tag="${TABLEPRO_RC_TAG:-linux-v0.1.0-rc1}"
remote="${TABLEPRO_RC_REMOTE:-https://github.com/cozygarage/TablePro.git}"

if [[ -n "$(git -C "$root" status --porcelain)" ]]; then
  echo "refusing to package a dirty tree; commit and validate the RC candidate first" >&2
  exit 1
fi
if ! git -C "$root" rev-parse --verify --quiet "refs/tags/$tag" >/dev/null; then
  echo "missing local RC tag: $tag" >&2
  exit 1
fi

commit="$(git -C "$root" rev-list -n 1 "$tag")"
if [[ "$commit" != "$(git -C "$root" rev-parse HEAD)" ]]; then
  echo "RC tag $tag does not identify the checked-out commit" >&2
  exit 1
fi

remote_refs="$(git ls-remote --tags "$remote" "refs/tags/$tag" "refs/tags/$tag^{}")"
remote_commit="$(awk '$2 ~ /\^\{\}$/ { print $1; found=1; exit } END { if (!found && NR == 1) print first } NR == 1 { first=$1 }' <<<"$remote_refs")"
if [[ -z "$remote_commit" || "$remote_commit" != "$commit" ]]; then
  echo "remote RC tag $tag does not resolve to local commit $commit" >&2
  exit 1
fi

archive="$(mktemp)"
trap 'rm -f -- "$archive"' EXIT
curl --fail --location --retry 3 \
  "https://github.com/cozygarage/TablePro/archive/${commit}.tar.gz" \
  --output "$archive"
checksum="$(sha256sum "$archive" | awk '{print $1}')"

cd "$root/packaging/arch"
export TABLEPRO_RC_COMMIT="$commit"
export TABLEPRO_RC_SHA256="$checksum"
makepkg --cleanbuild --clean --syncdeps --noconfirm
package_file="$(makepkg --packagelist)"
namcap PKGBUILD "$package_file"
"$root/scripts/validate-arch-package.sh" "$package_file"
