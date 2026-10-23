#!/usr/bin/env bash
# Enforce the macOS helper policy for a tag release.
#
# Usage: scripts/check-macos-app-status.sh [dist_dir]
#
# A tag release must ship the Developer ID signed and notarized app bundle.
# `ALLOW_UNSIGNED_RELEASE=1` relaxes the requirement for a pre-release.
# docs/PACKAGING.md describes the secrets and the fallback asset.
set -euo pipefail

dist=${1:-dist}
allow=${ALLOW_UNSIGNED_RELEASE:-0}

if [[ ! -d ${dist} ]]; then
  echo "dist directory not found: ${dist}" >&2
  exit 2
fi

signed_zip="${dist}/computer-use-macos-arm64-app.zip"
unsigned_zip="${dist}/computer-use-macos-arm64-app-unsigned.zip"
status="${dist}/computer-use-macos-app-status.txt"

if [[ ${allow} == "1" ]]; then
  echo "ALLOW_UNSIGNED_RELEASE=1: skipping the signed and notarized app requirement"
  exit 0
fi

if [[ ! -f ${signed_zip} ]]; then
  if [[ -f ${unsigned_zip} ]]; then
    echo "::error::This tag built only the ad-hoc signed macOS app. Tag releases require Developer ID signing and notarization. Set the repository variable ALLOW_UNSIGNED_RELEASE=1 only for a pre-release." >&2
  else
    echo "::error::This tag has no macOS app bundle. Tag releases require the Developer ID signed and notarized app; check the macOS packaging step." >&2
  fi
  exit 1
fi

if [[ ! -f ${status} ]]; then
  echo "::error::${status} is missing, so the macOS app cannot be proven signed and notarized." >&2
  exit 1
fi

sign_state=$(sed -n 's/^signed=//p' "${status}" | head -n 1)
notarized=$(sed -n 's/^notarized=//p' "${status}" | head -n 1)

if [[ ${sign_state} != "developer-id" ]]; then
  echo "::error::The macOS app is ${sign_state:-unknown}, not developer-id signed. Configure the signing secrets in docs/PACKAGING.md." >&2
  exit 1
fi
if [[ ${notarized} != "yes" ]]; then
  echo "::error::The macOS app was not notarized (notarized=${notarized:-unknown}). Configure the Apple ID secrets in docs/PACKAGING.md." >&2
  exit 1
fi

echo "macOS app verified: signed=${sign_state} notarized=${notarized}"
