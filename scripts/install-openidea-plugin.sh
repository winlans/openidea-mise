#!/usr/bin/env sh
set -eu

version="${OPENIDEA_CLI_VERSION:-0.1.47}"
install_dir="${1:-.tools}"
archive="openidea-plugin-linux-x86_64.gz"
url="https://github.com/l0ng-ai/openidea/releases/download/v${version}/${archive}"
tool="${install_dir}/openidea-plugin"
expected_sha256="${OPENIDEA_CLI_SHA256:-}"

if [ -z "${expected_sha256}" ]; then
  case "${version}" in
    0.1.47)
      expected_sha256="9394f2f4a892227f3ab17e8883e8deb340e8a6a8e9d8b61d7a9971ba51b70d1e"
      ;;
    *)
      echo "No trusted SHA-256 is configured for OpenIDEA CLI v${version}." >&2
      echo "Set OPENIDEA_CLI_SHA256 to the release asset digest." >&2
      exit 1
      ;;
  esac
fi

case "${expected_sha256}" in
  *[!0-9a-f]* | "")
    echo "OPENIDEA_CLI_SHA256 must be a lowercase 64-character SHA-256." >&2
    exit 1
    ;;
esac
if [ "${#expected_sha256}" -ne 64 ]; then
  echo "OPENIDEA_CLI_SHA256 must be a lowercase 64-character SHA-256." >&2
  exit 1
fi

case "$(uname -s):$(uname -m)" in
  Linux:x86_64) ;;
  *)
    echo "The pinned OpenIDEA release CLI is available for Linux x86_64 only." >&2
    exit 1
    ;;
esac

mkdir -p "${install_dir}"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT HUP INT TERM

echo "Downloading OpenIDEA plugin CLI v${version}"
curl --fail --location --retry 5 --retry-all-errors --silent --show-error \
  "${url}" --output "${tmp_dir}/${archive}"
actual_sha256="$(sha256sum "${tmp_dir}/${archive}" | cut -d ' ' -f 1)"
if [ "${actual_sha256}" != "${expected_sha256}" ]; then
  echo "OpenIDEA CLI archive SHA-256 mismatch." >&2
  echo "Expected ${expected_sha256}, got ${actual_sha256}." >&2
  exit 1
fi
gzip --decompress --stdout "${tmp_dir}/${archive}" > "${tmp_dir}/openidea-plugin"
chmod 0755 "${tmp_dir}/openidea-plugin"
mv "${tmp_dir}/openidea-plugin" "${tool}"
"${tool}" --help >/dev/null
echo "Installed ${tool}"
