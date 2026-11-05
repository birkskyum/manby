#!/bin/sh
# Renders og/card.html with headless Chrome: public/og.png, the Open Graph and
# Twitter image (1200x630 at twice the resolution), and og/github-social.png
# for the repository's social preview (1280x640, GitHub's size, under 1 MB).
set -e
cd "$(dirname "$0")"
CHROME=${CHROME:-"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"}
profile=$(mktemp -d)
render() { # width height scale output
	"$CHROME" --headless=new --disable-gpu --hide-scrollbars --allow-file-access-from-files \
		--user-data-dir="$profile" --force-device-scale-factor="$3" --window-size="$1,$2" \
		--virtual-time-budget=2000 --screenshot="$4" "file://$PWD/card.html" > /dev/null 2>&1
}
render 1200 630 2 ../public/og.png
render 1280 640 1 github-social.png
rm -rf "$profile"
ls -l ../public/og.png github-social.png
