#!/bin/sh
# Sets up the scratch directory for the golden tests: tests/ mirrors the
# tests of tippecanoe-rs, with every directory real, at every depth, and every
# file a symbolic link. The recipes write their outputs into those real
# directories, so nothing they write ends up under tippecanoe-rs. Files that
# earlier runs of tippecanoe-rs's own tests left behind there (tilesets,
# .check files and the like) are not linked, so that no recipe writes through
# a link into them.
#
# ./tippecanoe, ./tile-join and the other programs are manby, under the names
# the expected outputs record in generator_options. On Windows, where Git's
# bash runs this and ln -s copies, they are copies named with .exe.
#
#   golden/setup.sh <tippecanoe-rs tests> <scratch directory> <manby> <tippecanoe-rs release directory>
set -e
src=$(cd "$1" && pwd)
dst=$2
manby=$(cd "$(dirname "$3")" && pwd)/$(basename "$3")
bin=$(cd "$4" && pwd)
case $manby in
*.exe) exe=.exe ;;
*) exe= ;;
esac

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
ln -sf "$manby" "$dst/tippecanoe$exe"
ln -sf "$manby" "$dst/tile-join$exe"
ln -sf "$manby" "$dst/tippecanoe-decode$exe"
ln -sf "$manby" "$dst/tippecanoe-enumerate$exe"
ln -sf "$manby" "$dst/tippecanoe-json-tool$exe"
ln -sf "$manby" "$dst/tippecanoe-overzoom$exe"
ln -sf "$bin/examples/json2geobuf$exe" "$dst/json2geobuf$exe"

# tippecanoe records its command line in the metadata, and Git's bash starts a
# Windows program with its full path as its name: ./tippecanoe-decode puts the
# names that the expected outputs have back in what it prints.
if [ -n "$exe" ]; then
	cat >"$dst/tippecanoe-decode" <<'END'
#!/bin/bash
set -o pipefail
"$(dirname "$0")/tippecanoe-decode.exe" "$@" |
	sed -E "s#'[A-Za-z]:[^' ]*[\\\\/](tippecanoe|tile-join)\\.exe'#./\\1#g"
END
	chmod +x "$dst/tippecanoe-decode"
fi
