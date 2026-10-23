#!/bin/sh
# Sets up the scratch directory for the golden tests: tests/ mirrors the
# tests of tippecanoe-rs, with every directory real, at every depth, and every
# file a symbolic link. The recipes write their outputs into those real
# directories, so nothing they write ends up under tippecanoe-rs. Files that
# earlier runs of tippecanoe-rs's own tests left behind there (tilesets,
# .check files and the like) are not linked, so that no recipe writes through
# a link into them.
#
# ./tippecanoe is manby, under the name the expected outputs record in
# generator_options.
#
#   golden/setup.sh <tippecanoe-rs tests> <scratch directory> <manby> <tippecanoe-rs release directory>
set -e
src=$(cd "$1" && pwd)
dst=$2
manby=$(cd "$(dirname "$3")" && pwd)/$(basename "$3")
bin=$(cd "$4" && pwd)

# mirror <directory> <copy>
mirror() {
	mkdir -p "$2"
	for f in "$1"/* "$1"/.[!.]*; do
		[ -e "$f" ] || continue
		b=$(basename "$f")
		if [ -d "$f" ]; then
			mirror "$f" "$2/$b"
			continue
		fi
		case $b in
		*.mbtiles | *.check | *.geobuf | *.out | *.bak | .DS_Store) ;;
		*) ln -s "$f" "$2/$b" ;;
		esac
	done
}

rm -rf "$dst/tests"
mirror "$src" "$dst/tests"
ln -sf "$manby" "$dst/tippecanoe"
ln -sf "$bin/tippecanoe-decode" "$dst/tippecanoe-decode"
ln -sf "$bin/tippecanoe-json-tool" "$dst/tippecanoe-json-tool"
ln -sf "$bin/examples/json2geobuf" "$dst/json2geobuf"
