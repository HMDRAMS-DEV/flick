#!/bin/zsh
# Ships the version in Cargo.toml: builds the universal app, signs it with the Developer ID,
# packages a disk image, notarizes and staples it, and publishes it as a GitHub release.
#
#     scripts/release.sh "What changed, in a sentence or two."
#
# Needs the HMDFV Inc. Developer ID Application certificate in the login keychain and a
# notarytool profile named "ramihmd-notary":
#     xcrun notarytool store-credentials ramihmd-notary --apple-id <id> --team-id 8Z6WRF99H5
set -euo pipefail

repo=HMDRAMS-DEV/flick
identity="Developer ID Application: HMDFV Inc. (8Z6WRF99H5)"
notary=ramihmd-notary

cd "$(dirname "$0")/.."
notes="${1:?Usage: scripts/release.sh \"release notes\"}"
version=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
tag="v$version"
dmg="target/Flick.dmg"

[[ -z $(git status --porcelain) ]] || { echo "Commit or stash your changes first."; exit 1; }
if gh release view "$tag" -R "$repo" >/dev/null 2>&1; then echo "$tag is already released. Bump the version in Cargo.toml."; exit 1; fi

./build.sh
app="target/Flick.app"
codesign --force --timestamp --options runtime --sign "$identity" "$app"
codesign --verify --strict "$app"

stage="target/dmg"
rm -rf "$stage" "$dmg"
mkdir -p "$stage"
ditto "$app" "$stage/Flick.app"
ln -s /Applications "$stage/Applications"
hdiutil create -srcfolder "$stage" -volname Flick -fs HFS+ -format UDZO -imagekey zlib-level=9 -ov "$dmg" -quiet
rm -rf "$stage"
codesign --force --timestamp --sign "$identity" "$dmg"

# Stapling fails unless Apple accepted the image, which stops the release there.
xcrun notarytool submit "$dmg" --keychain-profile "$notary" --wait
xcrun stapler staple "$dmg"
spctl --assess --type open --context context:primary-signature -v "$dmg"

gh release create "$tag" "$dmg" -R "$repo" --target main --title "Flick $version" --notes "$notes"
echo "Released Flick $version: https://github.com/$repo/releases/tag/$tag"
