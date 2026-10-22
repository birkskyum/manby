#!/bin/bash
# Times manby against tippecanoe on the benchmarks, alternating runs, and checks
# that manby's tiles decode to the same output as tippecanoe's.
#
#   bench/run.sh [benchmark ...]     (default: points tracts tracts-z12 roads-z14)
#
# TIPPECANOE: the reference tippecanoe (default: ../tippecanoe-rs/target/release/tippecanoe)
# MANBY: the manby to test (default: target/release/manby)
# ROUNDS: runs per program (default 3)
# MANBY_FLAGS: extra flags for manby only, such as --fast
cd "$(dirname "$0")/.."
TIPPECANOE=${TIPPECANOE:-../tippecanoe-rs/target/release/tippecanoe}
DECODE=${DECODE:-../tippecanoe-rs/target/release/tippecanoe-decode}
MANBY=${MANBY:-target/release/manby}
ROUNDS=${ROUNDS:-3}
D=bench/data
O=bench/out
mkdir -p $O

args_for() {
	case $1 in
	points) echo "-zg --drop-densest-as-needed -P $D/points4m.geojsonl" ;;
	tracts) echo "-zg --coalesce-densest-as-needed -P $D/cb_2023_us_tract_500k.geojsonl" ;;
	tracts-z12) echo "-z12 -Z0 --coalesce-densest-as-needed -P $D/cb_2023_us_tract_500k.geojsonl" ;;
	roads-z14) echo "-z14 -Z0 --drop-densest-as-needed -P $D/tl_2024_us_primaryroads.geojsonl" ;;
	esac
}

# Prints "wall user sys" in seconds for one run.
timed() {
	if ! /usr/bin/time -p "$@" > /dev/null 2> $O/time.err; then
		echo "FAILED: $*" >&2
		tail -5 $O/time.err >&2
		echo "nan nan nan"
		return
	fi
	awk '/^real/{r=$2} /^user/{u=$2} /^sys/{s=$2} END{print r, u, s}' $O/time.err
}

names="$*"
[ -z "$names" ] && names="points tracts tracts-z12 roads-z14"

printf "%-12s %-10s %8s %8s %8s\n" benchmark program wall user sys
for name in $names; do
	args=$(args_for $name)
	[ -z "$args" ] && { echo "unknown benchmark $name"; continue; }
	for round in $(seq 1 $ROUNDS); do
		read w u s < <(timed $TIPPECANOE -q -f -o $O/tippecanoe-$name.pmtiles $args)
		printf "%-12s %-10s %8s %8s %8s\n" $name tippecanoe $w $u $s
		read w u s < <(timed $MANBY -q -f -o $O/manby-$name.pmtiles $MANBY_FLAGS $args)
		printf "%-12s %-10s %8s %8s %8s\n" $name manby $w $u $s
	done
	strip="-x generator -x generator_options -x name -x description"
	if cmp -s <($DECODE $strip $O/tippecanoe-$name.pmtiles) <($DECODE $strip $O/manby-$name.pmtiles); then
		echo "$name: same tiles as tippecanoe"
	else
		echo "$name: TILES DIFFER from tippecanoe"
	fi
done
