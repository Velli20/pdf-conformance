#!/usr/bin/env bash
# Downloads Mozilla's pdfjs-dist package from the npm registry, checks it against its published
# integrity, unpacks it into <dir> and points the harness at it through PDFJS_DIST.
#   install-pdfjs.sh <dir>
# Needs PDFJS_VERSION and PDFJS_INTEGRITY (the registry's `sha512-…` value).
set -euo pipefail

dir=${1:?usage: install-pdfjs.sh <dir>}
version=${PDFJS_VERSION:?}
mkdir -p "$dir"
curl -sSfL -o "$dir/pdfjs-dist.tgz" "https://registry.npmjs.org/pdfjs-dist/-/pdfjs-dist-$version.tgz"
actual="sha512-$(openssl dgst -sha512 -binary "$dir/pdfjs-dist.tgz" | base64 | tr -d '\n')"
if [ "$actual" != "${PDFJS_INTEGRITY:?}" ]; then
  echo "::error ::pdfjs-dist $version does not match its pinned integrity ($actual)"
  exit 1
fi
tar -xzf "$dir/pdfjs-dist.tgz" -C "$dir"
echo "PDFJS_DIST=$dir/package" >> "${GITHUB_ENV:?}"
echo "pdf.js $version in $dir/package"
