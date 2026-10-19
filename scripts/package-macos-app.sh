#!/usr/bin/env bash
# Build Computer Use.app, optionally Developer ID sign and notarize it, and zip it.
# See docs/PACKAGING.md.
#
# Usage:
#   scripts/package-macos-app.sh [binary] [output_dir]
#
# binary defaults to ${COMPUTER_USE_BINARY:-target/release/computer-use}.
# output_dir defaults to ${COMPUTER_USE_OUTPUT_DIR:-dist}.
# Relative paths are resolved from the current working directory.
#
# MACOS_SIGN_IDENTITY
#   When set, codesign with hardened runtime, a timestamp, and the entitlements.
#   When unset, the bundle is ad-hoc signed (codesign --sign -) so local TCC
#   has a stable code requirement. The zip is then named *-unsigned.zip.
# APPLE_ID, APPLE_APP_SPECIFIC_PASSWORD, APPLE_TEAM_ID
#   When all three are set and a signing identity was used, notarize and staple.
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd "${script_dir}/.." && pwd)

binary=${1:-${COMPUTER_USE_BINARY:-target/release/computer-use}}
output_dir=${2:-${COMPUTER_USE_OUTPUT_DIR:-dist}}

if [[ ${binary} != /* ]]; then
  binary="${PWD}/${binary}"
fi
if [[ ${output_dir} != /* ]]; then
  output_dir="${PWD}/${output_dir}"
fi

if [[ ! -f ${binary} ]]; then
  echo "binary not found: ${binary}" >&2
  exit 1
fi
if [[ ! -x ${binary} ]]; then
  echo "binary is not executable: ${binary}" >&2
  exit 1
fi

cargo_toml="${repo_root}/Cargo.toml"
version=$(sed -n '1,/^\[dependencies\]/s/^version = "\([^"]*\)"/\1/p' "${cargo_toml}")
version=${version%%$'\n'*}
if [[ -z ${version} ]]; then
  echo "could not read version from ${cargo_toml}" >&2
  exit 1
fi
bundle_version=${version%%-*}
if [[ ! ${bundle_version} =~ ^[0-9]+(\.[0-9]+){0,2}$ ]]; then
  echo "CFBundleVersion cannot use ${version}; expected up to three numeric components" >&2
  exit 1
fi

app="${output_dir}/Computer Use.app"
rm -rf "${app}"
mkdir -p "${app}/Contents/MacOS" "${app}/Contents/Resources"
cp "${binary}" "${app}/Contents/MacOS/computer-use"
chmod +x "${app}/Contents/MacOS/computer-use"

template="${repo_root}/packaging/macos/Info.plist"
entitlements="${repo_root}/packaging/macos/entitlements.plist"
sed "s/@CARGO_VERSION@/${bundle_version}/g" "${template}" > "${app}/Contents/Info.plist"
if grep -q '@CARGO_VERSION@' "${app}/Contents/Info.plist"; then
  echo "Info.plist still contains @CARGO_VERSION@" >&2
  exit 1
fi

signed=0
if [[ -n ${MACOS_SIGN_IDENTITY:-} ]]; then
  codesign --deep --force --options runtime --timestamp \
    --entitlements "${entitlements}" \
    --sign "${MACOS_SIGN_IDENTITY}" \
    "${app}"
  signed=1
else
  # Ad-hoc. TCC on this Mac can bind to the bundle identifier.
  codesign --force --sign - "${app}/Contents/MacOS/computer-use"
  codesign --force --sign - "${app}"
fi

notarized=no
if [[ ${signed} -eq 1 && -n ${APPLE_ID:-} && -n ${APPLE_APP_SPECIFIC_PASSWORD:-} && -n ${APPLE_TEAM_ID:-} ]]; then
  notarize_zip="${output_dir}/computer-use-notarize.zip"
  rm -f "${notarize_zip}"
  ditto -c -k --keepParent "${app}" "${notarize_zip}"
  xcrun notarytool submit "${notarize_zip}" \
    --apple-id "${APPLE_ID}" \
    --password "${APPLE_APP_SPECIFIC_PASSWORD}" \
    --team-id "${APPLE_TEAM_ID}" \
    --wait
  xcrun stapler staple "${app}"
  rm -f "${notarize_zip}"
  notarized=yes
fi

if [[ ${signed} -eq 1 ]]; then
  zip_name="computer-use-macos-arm64-app.zip"
  sign_state="developer-id"
else
  zip_name="computer-use-macos-arm64-app-unsigned.zip"
  sign_state="adhoc"
fi

mkdir -p "${output_dir}"
archive="${output_dir}/${zip_name}"
rm -f "${archive}"
ditto -c -k --keepParent "${app}" "${archive}"
printf 'signed=%s\nnotarized=%s\n' "${sign_state}" "${notarized}" > "${output_dir}/computer-use-macos-app-status.txt"
echo "${archive}"
