#!/bin/bash
# Runs manby on inputs several times larger than a memory budget, and checks
# that its tiles match tippecanoe's and that its peak memory stays near the
# budget.
#
#   bench/large.sh [benchmark ...]     (default: points40m tracts20)
#
# points40m: 40 million clustered points (points.py), 5.6 GB
# tracts20:  the census tracts 20 times, moved north and south (repeat.py), 3 GB
#
# The inputs are made in bench/data on the first run. tippecanoe runs once per
# benchmark, as its tiles are the reference; set TIPPECANOE_AGAIN=1 to run it
# again, for its time and memory.
#
# BUDGET: manby's --memory-budget (default 1G)
# TIPPECANOE, DECODE, MANBY, OUT: as in run.sh
cd "$(dirname "$0")/.."
TIPPECANOE=${TIPPECANOE:-../tippecanoe-rs/target/release/tippecanoe}
DECODE=${DECODE:-../tippecanoe-rs/target/release/tippecanoe-decode}
MANBY=${MANBY:-target/release/manby}
BUDGET=${BUDGET:-1G}
D=bench/data
O=${OUT:-bench/out}
mkdir -p $D $O

make_input() {
	case $1 in
	points40m) [ -s $D/points40m.geojsonl ] || python3 bench/points.py 40000000 > $D/points40m.geojsonl ;;
	tracts20)
		[ -s $D/cb_2023_us_tract_500k.geojsonl ] || bench/fetch-data.sh
		[ -s $D/tracts20.geojsonl ] || python3 bench/repeat.py $D/cb_2023_us_tract_500k.geojsonl 20 > $D/tracts20.geojsonl
		;;
	esac
}

args_for() {
	case $1 in
	points40m) echo "-zg --drop-densest-as-needed -P $D/points40m.geojsonl" ;;
	tracts20) echo "-zg --coalesce-densest-as-needed -P $D/tracts20.geojsonl" ;;
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

names="$*"
[ -z "$names" ] && names="points40m tracts20"

printf "%-10s %-22s %10s %10s\n" benchmark program wall peak-MB
for name in $names; do
	args=$(args_for $name)
	[ -z "$args" ] && { echo "unknown benchmark $name"; continue; }
	make_input $name
	if [ ! -s $O/tippecanoe-$name.pmtiles ] || [ -n "$TIPPECANOE_AGAIN" ]; then
		read w m < <(measured $TIPPECANOE -q -f -o $O/tippecanoe-$name.pmtiles $args)
		printf "%-10s %-22s %10s %10s\n" $name tippecanoe $w $m
	fi
	read w m < <(measured $MANBY -q -f -o $O/manby-$name.pmtiles --memory-budget=$BUDGET $args)
	printf "%-10s %-22s %10s %10s\n" $name "manby, budget $BUDGET" $w $m
	strip="-x generator -x generator_options -x name -x description"
	if cmp -s <($DECODE $strip $O/tippecanoe-$name.pmtiles) <($DECODE $strip $O/manby-$name.pmtiles); then
		echo "$name: same tiles as tippecanoe"
	else
		echo "$name: TILES DIFFER from tippecanoe"
	fi
done
