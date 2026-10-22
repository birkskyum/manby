#!/bin/sh
# Sets up the scratch directory for the golden tests: tests/ mirrors the
# tests of tippecanoe-rs with symbolic links, except that each out/ directory
# is real, so that the files the recipes write stay here. ./tippecanoe is
# manby, under the name the expected outputs record in generator_options.
#
#   golden/setup.sh <tippecanoe-rs tests> <scratch directory> <manby> <tippecanoe-rs release directory>
set -e
src=$(cd "$1" && pwd)
dst=$2
manby=$(cd "$(dirname "$3")" && pwd)/$(basename "$3")
bin=$(cd "$4" && pwd)

rm -rf "$dst/tests"
mkdir -p "$dst/tests"
for dir in "$src"/*/; do
	name=$(basename "$dir")
	mkdir -p "$dst/tests/$name"
	for f in "$dir"* "$dir".[!.]*; do
		[ -e "$f" ] || continue
		b=$(basename "$f")
		if [ "$b" = out ] && [ -d "$f" ]; then
			mkdir -p "$dst/tests/$name/out"
			for o in "$f"/*; do
				[ -e "$o" ] && ln -s "$o" "$dst/tests/$name/out/$(basename "$o")"
			done
		else
			ln -s "$f" "$dst/tests/$name/$b"
		fi
	done
done
ln -sf "$manby" "$dst/tippecanoe"
ln -sf "$bin/tippecanoe-decode" "$dst/tippecanoe-decode"
ln -sf "$bin/tippecanoe-json-tool" "$dst/tippecanoe-json-tool"
ln -sf "$bin/examples/json2geobuf" "$dst/json2geobuf"
