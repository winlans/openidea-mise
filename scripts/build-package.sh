#!/usr/bin/env sh
set -eu

expected_version="${1:-}"
cli="${OPENIDEA_PLUGIN_BIN:-.tools/openidea-plugin}"
key_file="${OPENIDEA_SIGNING_KEY_FILE:-}"

if [ -z "${expected_version}" ]; then
  echo "usage: scripts/build-package.sh <version>" >&2
  exit 2
fi
if [ ! -x "${cli}" ]; then
  echo "OpenIDEA plugin CLI is not executable: ${cli}" >&2
  exit 1
fi
if [ -z "${key_file}" ] || [ ! -f "${key_file}" ]; then
  echo "OPENIDEA_SIGNING_KEY_FILE must name the publisher key file" >&2
  exit 1
fi

python3 scripts/check_versions.py --expected "${expected_version}"
package="winlans.mise-${expected_version}.oiplugin"
mkdir -p dist

"${cli}" build .
"${cli}" check .
"${cli}" pack . \
  --key "${key_file}" \
  --publisher winlans \
  --out "dist/${package}"
"${cli}" check "dist/${package}"

sha256="$(sha256sum "dist/${package}" | cut -d ' ' -f 1)"
printf '%s  %s\n' "${sha256}" "${package}" > "dist/${package}.sha256"

if [ -n "${GITHUB_OUTPUT:-}" ]; then
  {
    echo "package=${package}"
    echo "package_path=dist/${package}"
    echo "checksum_path=dist/${package}.sha256"
    echo "sha256=${sha256}"
  } >> "${GITHUB_OUTPUT}"
fi

echo "Built dist/${package}"
echo "SHA-256 ${sha256}"
