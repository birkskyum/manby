#!/bin/bash
# Times manby against tippecanoe on the benchmarks, alternating runs, with wall
# time and peak resident memory, and checks that manby's tiles decode to the
# same output as tippecanoe's.
#
#   bench/run.sh [benchmark ...]     (default: points tracts tracts-z12 roads-z14 points-cluster)
#
# points           the 4 million points, -zg --drop-densest-as-needed
# tracts           the census tracts, -zg --coalesce-densest-as-needed
# tracts-z12       the tracts at -z12 (312,073 tiles)
# roads-z14        the primary roads at -z14 (122,725 tiles)
# points-cluster   the points clustered within 10 pixels, summing value
# cluster-densest  the points, -zg --cluster-densest-as-needed
# cluster-r1-sum   the points, -zg -r1 --cluster-densest-as-needed, summing value
#
# TIPPECANOE: the reference tippecanoe (default: ../tippecanoe-rs/target/release/tippecanoe),
#   or tippecanoe's own C++ build
# DECODE: the tippecanoe-decode that compares the tiles (default: tippecanoe-rs's)
# MANBY: the manby to test (default: target/release/manby)
# ROUNDS: runs per program (default 3); the summary has their medians
# MANBY_FLAGS: extra flags for manby only, such as --fast
# OUT: the directory for the tilesets (default: bench/out)
cd "$(dirname "$0")/.."
TIPPECANOE=${TIPPECANOE:-../tippecanoe-rs/target/release/tippecanoe}
DECODE=${DECODE:-../tippecanoe-rs/target/release/tippecanoe-decode}
MANBY=${MANBY:-target/release/manby}
ROUNDS=${ROUNDS:-3}
D=bench/data
O=${OUT:-bench/out}
mkdir -p $O

args_for() {
	case $1 in
	points) echo "-zg --drop-densest-as-needed -P $D/points4m.geojsonl" ;;
	tracts) echo "-zg --coalesce-densest-as-needed -P $D/cb_2023_us_tract_500k.geojsonl" ;;
	tracts-z12) echo "-z12 -Z0 --coalesce-densest-as-needed -P $D/cb_2023_us_tract_500k.geojsonl" ;;
	roads-z14) echo "-z14 -Z0 --drop-densest-as-needed -P $D/tl_2024_us_primaryroads.geojsonl" ;;
	points-cluster) echo "-zg -r1 --cluster-distance=10 --cluster-densest-as-needed --accumulate-attribute=value:sum -P $D/points4m.geojsonl" ;;
	cluster-densest) echo "-zg --cluster-densest-as-needed -P $D/points4m.geojsonl" ;;
	cluster-r1-sum) echo "-zg -r1 --cluster-densest-as-needed --accumulate-attribute=value:sum -P $D/points4m.geojsonl" ;;
	esac
}

# Prints "wall peak-MB" for one run.
measured() {
	if [ "$(uname)" = Darwin ]; then
		/usr/bin/time -l "$@" > /dev/null 2> $O/time.err || { tail -5 $O/time.err >&2; echo "nan nan"; return; }
		awk '/ real/{w=$1} /maximum resident/{m=$1/1048576} END{printf "%s %.0f\n", w, m}' $O/time.err
	else
		/usr/bin/time -f "%e %M" "$@" > /dev/null 2> $O/time.err || { tail -5 $O/time.err >&2; echo "nan nan"; return; }
		tail -1 $O/time.err | awk '{printf "%s %.0f\n", $1, $2/1024}'
	fi
}

median() {
	tr ' ' '\n' | grep -v '^$' | sort -g | awk '{v[NR]=$1} END{print v[int((NR+1)/2)]}'
}

names="$*"
[ -z "$names" ] && names="points tracts tracts-z12 roads-z14 points-cluster"

summary=""
printf "%-16s %-10s %10s %10s\n" benchmark program wall peak-MB
for name in $names; do
	args=$(args_for $name)
	[ -z "$args" ] && { echo "unknown benchmark $name"; continue; }
	tw="" tm="" mw="" mm=""
	for round in $(seq 1 $ROUNDS); do
		read w m < <(measured $TIPPECANOE -q -f -o $O/tippecanoe-$name.pmtiles $args)
		printf "%-16s %-10s %10s %10s\n" $name tippecanoe $w $m
		tw="$tw $w" tm="$tm $m"
		read w m < <(measured $MANBY -q -f -o $O/manby-$name.pmtiles $MANBY_FLAGS $args)
		printf "%-16s %-10s %10s %10s\n" $name manby $w $m
		mw="$mw $w" mm="$mm $m"
	done
	strip="-x generator -x generator_options -x name -x description"
	if cmp -s <($DECODE $strip $O/tippecanoe-$name.pmtiles) <($DECODE $strip $O/manby-$name.pmtiles); then
		echo "$name: same tiles as tippecanoe"
	else
		echo "$name: TILES DIFFER from tippecanoe"
	fi
	t=$(echo $tw | median) m=$(echo $mw | median)
	summary="$summary$(printf "%-16s %8.2f s %6s MB %8.2f s %6s MB %6.1fx" $name $t $(echo $tm | median) $m $(echo $mm | median) $(echo "$t / $m" | bc -l))
"
done

echo
echo "medians of $ROUNDS runs"
printf "%-16s %20s %20s %7s\n" benchmark tippecanoe manby speedup
printf "%s" "$summary"
