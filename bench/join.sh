#!/bin/bash
# Times manby join against tippecanoe-rs's tile-join, alternating runs, with
# wall time and peak resident memory, and checks that manby join's tiles
# decode to the same output as tile-join's.
#
#   bench/join.sh [benchmark ...]
#
# copy           the census tracts at -z12 (312,073 tiles) copied to PMTiles
# copy-mbtiles   the same, to MBTiles
# csv            the same tracts joined to a CSV keyed on GEOID (tracts-csv.py)
# merge          the 4 million points and the tracts, both -zg, into PMTiles
# merge-mbtiles  the same, to MBTiles
# overzoom       counties at -z5 and tracts at -z9, with --overzoom
#
# The inputs are made in bench/out/join on the first run. The copy and the CSV
# join read a tileset manby made. Where input tiles share a z/x/y, as in merge
# and overzoom, the order of their features depends on the bytes of the stored
# tiles, so those inputs are made with tippecanoe-rs, whose bytes both programs
# then see. For those two, the script also checks the whole pipeline: manby join
# on inputs that manby made against tile-join on inputs that tippecanoe made.
#
# TILE_JOIN, TIPPECANOE, DECODE: tippecanoe-rs's programs (default ../tippecanoe-rs/target/release)
# MANBY: the manby to test (default target/release/manby)
# ROUNDS: runs per program (default 3); the summary has their medians
cd "$(dirname "$0")/.."
RS=../tippecanoe-rs/target/release
TILE_JOIN=${TILE_JOIN:-$RS/tile-join}
TIPPECANOE=${TIPPECANOE:-$RS/tippecanoe}
DECODE=${DECODE:-$RS/tippecanoe-decode}
MANBY=${MANBY:-target/release/manby}
ROUNDS=${ROUNDS:-3}
D=bench/data
O=bench/out
J=$O/join
mkdir -p $J

# make_tiles <output> <program> <args...>
make_tiles() {
	local out=$1 program=$2
	shift 2
	[ -s $out ] && return
	echo "making $out" >&2
	$program -q -f -o $out "$@" || exit 1
}

make_input() {
	case $1 in
	copy | copy-mbtiles)
		make_tiles $J/manby-tracts-z12.pmtiles $MANBY -z12 -Z0 --coalesce-densest-as-needed -P $D/cb_2023_us_tract_500k.geojsonl
		;;
	csv)
		make_input copy
		[ -s $J/tracts.csv ] || python3 -I bench/tracts-csv.py $D/cb_2023_us_tract_500k.geojsonl > $J/tracts.csv
		;;
	merge | merge-mbtiles)
		for p in tippecanoe:$TIPPECANOE manby:$MANBY; do
			make_tiles $J/${p%%:*}-points.pmtiles ${p#*:} -zg --drop-densest-as-needed -P $D/points4m.geojsonl
			make_tiles $J/${p%%:*}-tracts.pmtiles ${p#*:} -zg --coalesce-densest-as-needed -P $D/cb_2023_us_tract_500k.geojsonl
		done
		;;
	overzoom)
		for p in tippecanoe:$TIPPECANOE manby:$MANBY; do
			make_tiles $J/${p%%:*}-counties-z5.pmtiles ${p#*:} -z5 -P -l counties $D/tl_2024_us_county.geojsonl
			make_tiles $J/${p%%:*}-tracts-z9.pmtiles ${p#*:} -z9 --coalesce-densest-as-needed -P -l tracts $D/cb_2023_us_tract_500k.geojsonl
		done
		;;
	esac
}

# The arguments for a benchmark, with $made for the program that made the
# inputs where both did.
args_for() {
	local made=${2:-tippecanoe}
	case $1 in
	copy) echo "$J/manby-tracts-z12.pmtiles" ;;
	copy-mbtiles) echo "$J/manby-tracts-z12.pmtiles" ;;
	csv) echo "-c $J/tracts.csv $J/manby-tracts-z12.pmtiles" ;;
	merge | merge-mbtiles) echo "$J/$made-points.pmtiles $J/$made-tracts.pmtiles" ;;
	overzoom) echo "--overzoom $J/$made-counties-z5.pmtiles $J/$made-tracts-z9.pmtiles" ;;
	esac
}

suffix_for() {
	case $1 in
	*-mbtiles) echo mbtiles ;;
	*) echo pmtiles ;;
	esac
}

# Prints "wall peak-MB" for one run.
measured() {
	if [ "$(uname)" = Darwin ]; then
		/usr/bin/time -l "$@" > /dev/null 2> $O/time.err || { tail -5 $O/time.err >&2; echo "nan nan"; return; }
		awk '/ real/{w=$1} /maximum resident/{m=$1/1048576} END{printf "%s %.0f\n", w, m}' $O/time.err
	else
		/usr/bin/time -v "$@" > /dev/null 2> $O/time.err || { tail -5 $O/time.err >&2; echo "nan nan"; return; }
		awk -F': ' '/Elapsed/{w=$2} /Maximum resident/{m=$2/1024} END{printf "%s %.0f\n", w, m}' $O/time.err
	fi
}

median() {
	tr ' ' '\n' | grep -v '^$' | sort -g | awk '{v[NR]=$1} END{print v[int((NR+1)/2)]}'
}

strip="-x generator -x generator_options -x name -x description"
same() {
	cmp -s <($DECODE $strip $1) <($DECODE $strip $2)
}

names="$*"
[ -z "$names" ] && names="copy copy-mbtiles csv merge merge-mbtiles overzoom"

summary=""
printf "%-14s %-10s %10s %10s\n" benchmark program wall peak-MB
for name in $names; do
	args=$(args_for $name)
	[ -z "$args" ] && { echo "unknown benchmark $name"; continue; }
	make_input $name
	ext=$(suffix_for $name)
	tw="" tm="" mw="" mm=""
	for round in $(seq 1 $ROUNDS); do
		read w m < <(measured $TILE_JOIN -q -f -o $O/tile-join-$name.$ext $args)
		printf "%-14s %-10s %10s %10s\n" $name tile-join $w $m
		tw="$tw $w" tm="$tm $m"
		read w m < <(measured $MANBY join -q -f -o $O/manby-join-$name.$ext $args)
		printf "%-14s %-10s %10s %10s\n" $name "manby join" $w $m
		mw="$mw $w" mm="$mm $m"
	done
	if same $O/tile-join-$name.$ext $O/manby-join-$name.$ext; then
		check="same tiles"
	else
		check="TILES DIFFER"
	fi
	case $name in
	merge | overzoom)
		# manby, then manby join, against tippecanoe, then tile-join
		$MANBY join -q -f -o $O/manby-join-$name-pipeline.$ext $(args_for $name manby) || exit 1
		if same $O/tile-join-$name.$ext $O/manby-join-$name-pipeline.$ext; then
			check="$check; from manby's inputs, same tiles as tile-join from tippecanoe's"
		else
			check="$check; from manby's inputs, TILES DIFFER from tile-join from tippecanoe's"
		fi
		;;
	esac
	echo "$name: $check"
	t=$(echo $tw | median) m=$(echo $mw | median)
	summary="$summary$(printf "%-14s %8.2f s %6s MB %8.2f s %6s MB %6.1fx" $name $t $(echo $tm | median) $m $(echo $mm | median) $(echo "$t / $m" | bc -l))
"
done

echo
echo "medians of $ROUNDS runs"
printf "%-14s %20s %20s %7s\n" benchmark tile-join "manby join" speedup
printf "%s" "$summary"
