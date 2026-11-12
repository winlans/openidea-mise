#!/usr/bin/env sh
set -eu

cli="${OPENIDEA_PLUGIN_BIN:-.tools/openidea-plugin}"

if [ ! -x "${cli}" ]; then
  echo "OpenIDEA plugin CLI is not executable: ${cli}" >&2
  echo "Run 'mise run setup' first." >&2
  exit 1
fi

version="$(python3 scripts/check_versions.py --print-version)"
package="winlans.mise-${version}-local.oiplugin"
package_path="dist/${package}"
checksum_path="${package_path}.sha256"

mkdir -p dist

"${cli}" build .
"${cli}" check .
"${cli}" pack . --out "${package_path}"
"${cli}" check "${package_path}"

sha256="$(sha256sum "${package_path}" | cut -d ' ' -f 1)"
printf '%s  %s\n' "${sha256}" "${package_path}" > "${checksum_path}"

echo
echo "Local install package: $(pwd)/${package_path}"
echo "Checksum file:        $(pwd)/${checksum_path}"
echo "SHA-256:              ${sha256}"
