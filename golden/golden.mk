# tippecanoe's .json.check recipes, run with manby. This runs in the scratch
# directory that the Makefile's test-golden sets up, where ./tippecanoe is
# manby and tests/ mirrors tippecanoe-rs's tests with the outputs written locally.

# Make patterns treat % specially, so the skip list is applied in the shell.
TESTS := $(shell for f in tests/*/out/*.json; do grep -qxF "$$f" skip.txt || echo "$$f"; done)
SPACE = $(NULL) $(NULL)

suffixes = json json.gz

GOLDEN_MK := $(abspath $(firstword $(MAKEFILE_LIST)))
ADVSHELL = /bin/bash

# The other recipes of tippecanoe's tests that need only tippecanoe and tippecanoe-decode
RECIPES = csv-test flatgeobuf-test layer-json-test parallel-test radix-sort-test
# and those of tile-join, with ./tile-join manby too, and tippecanoe-enumerate.
# decode-test and decode-pmtiles-test are not here: they compare the output of
# tippecanoe-decode --stats, which has the compressed size of each tile, and
# manby's tiles are compressed with libdeflate. allow-existing-test is not
# either: its expected output is stale, and tippecanoe and tippecanoe-rs fail
# it the same way (see tippecanoe-rs's README.md).
RECIPES += raw-tiles-test pmtiles-test enumerate-test join-test join-filter-test
# and those of tippecanoe's other programs, all of them manby too.
RECIPES += pbf-test overzoom-test json-tool-test

all: $(addsuffix .check,$(TESTS)) $(RECIPES)

# Work around Makefile and filename punctuation limits:
# _ for argument-separator space
# %20 for quoted space
# %22 for quoted quote
# %2f for /
# %3a for :
# %3d for =
# %5f for _
# %7b for {

testargs = \
    $(subst %20,' ',\
        $(subst %22,'"',\
            $(subst %3a,:,\
                $(subst %3d,=,\
                    $(subst %2f,/,\
                        $(subst %7b,'{',\
                            $(subst %5f,'_',\
                                $(subst _, ,$(1)))))))))

%.json.check:
	./tippecanoe -q -a@ -f -o $@.mbtiles $(call testargs,$(patsubst %.json.check,%,$(word 4,$(subst /, ,$@)))) $(foreach suffix,$(suffixes),$(sort $(wildcard $(subst $(SPACE),/,$(wordlist 1,2,$(subst /, ,$@)))/*.$(suffix)))) < /dev/null
	./tippecanoe-decode -x generator $@.mbtiles > $@.out
	cmp $@.out $(patsubst %.check,%,$@)
	rm $@.out $@.mbtiles

# The recipes below are copied from tippecanoe's Makefile.

radix-sort-test: tippecanoe tippecanoe-decode
	mkdir -p tests/radix-sort
	perl -e 'for ($$f = 0; $$f < 8; $$f++) { print "{ \"type\": \"Feature\", \"properties\": { \"f\": $$f }, \"geometry\": { \"type\": \"LineString\", \"coordinates\": ["; for ($$i = 0; $$i < 2000; $$i++) { print "," unless $$i == 0; printf "[%f,%f]", $$f * 40 - 175 + $$i * 0.001, $$i % 2 * 0.5 - 20; } print "] } }\n"; }' > tests/radix-sort/bigfeatures.json
	perl -e 'for ($$i = 0; $$i < 500; $$i++) { print "{ \"type\": \"Feature\", \"properties\": { \"i\": $$i }, \"geometry\": { \"type\": \"Point\", \"coordinates\": [ 17, 42 ] } }\n"; }' > tests/radix-sort/onelocation.json
	$(MAKE) -f $(GOLDEN_MK) radix-sort-compare RADIXIN="tests/radix-sort/bigfeatures.json" RADIXARGS="-z4"
	$(MAKE) -f $(GOLDEN_MK) radix-sort-compare RADIXIN="tests/radix-sort/onelocation.json" RADIXARGS="-z4"
	$(MAKE) -f $(GOLDEN_MK) radix-sort-compare RADIXIN="tests/feature-filter/in.json" RADIXARGS="-z4"
	$(MAKE) -f $(GOLDEN_MK) radix-sort-compare RADIXIN="tests/ne_110m_ocean/in.json" RADIXARGS="-z4"
	$(MAKE) -f $(GOLDEN_MK) radix-sort-compare RADIXIN="tests/border/in.json" RADIXARGS="-z4"
	$(MAKE) -f $(GOLDEN_MK) radix-sort-compare RADIXIN="tests/loop/in.json" RADIXARGS="-z4"
	$(MAKE) -f $(GOLDEN_MK) radix-sort-compare RADIXIN="tests/tl_2022_11_tract/in.json.gz" RADIXARGS="-z4"
	$(MAKE) -f $(GOLDEN_MK) radix-sort-compare RADIXIN="tests/epsg-3857/in.json" RADIXARGS="-z4 -sEPSG:3857"
	rm -r tests/radix-sort

radix-sort-compare:
	./tippecanoe -q -f $(RADIXARGS) -o tests/radix-sort/memory.mbtiles $(RADIXIN)
	./tippecanoe -q -f $(RADIXARGS) -aR -o tests/radix-sort/radix.mbtiles $(RADIXIN)
	./tippecanoe-decode -x generator -x generator_options -x name -x description tests/radix-sort/memory.mbtiles > tests/radix-sort/memory.json
	./tippecanoe-decode -x generator -x generator_options -x name -x description tests/radix-sort/radix.mbtiles > tests/radix-sort/radix.json
	cmp tests/radix-sort/memory.json tests/radix-sort/radix.json
	rm tests/radix-sort/memory.mbtiles tests/radix-sort/radix.mbtiles tests/radix-sort/memory.json tests/radix-sort/radix.json

parallel-test: $(eval SHELL:=$(ADVSHELL))
	mkdir -p tests/parallel
	perl -e 'for ($$i = 0; $$i < 20; $$i++) { $$lon = rand(360) - 180; $$lat = rand(180) - 90; $$k = rand(1); $$v = rand(1); print "{ \"type\": \"Feature\", \"properties\": { \"yes\": \"no\", \"who\": 1, \"$$k\": \"$$v\" }, \"geometry\": { \"type\": \"Point\", \"coordinates\": [ $$lon, $$lat ] } }\n"; }' > tests/parallel/in1.json
	perl -e 'for ($$i = 0; $$i < 300000; $$i++) { $$lon = rand(360) - 180; $$lat = rand(180) - 90; print "{ \"type\": \"Feature\", \"properties\": { }, \"geometry\": { \"type\": \"Point\", \"coordinates\": [ $$lon, $$lat ] } }\n"; }' > tests/parallel/in2.json
	perl -e 'for ($$i = 0; $$i < 20; $$i++) { $$lon = rand(360) - 180; $$lat = rand(180) - 90; print "{ \"type\": \"Feature\", \"properties\": { }, \"geometry\": { \"type\": \"Point\", \"coordinates\": [ $$lon, $$lat ] } }\n"; }' > tests/parallel/in3.json
	perl -e 'for ($$i = 0; $$i < 20; $$i++) { $$lon = rand(360) - 180; $$lat = rand(180) - 90; $$v = rand(1); print "{ \"type\": \"Feature\", \"properties\": { }, \"tippecanoe\": { \"layer\": \"$$v\" }, \"geometry\": { \"type\": \"Point\", \"coordinates\": [ $$lon, $$lat ] } }\n"; }' > tests/parallel/in4.json
	echo -n "" > tests/parallel/empty1.json
	echo "" > tests/parallel/empty2.json
	./tippecanoe -q -z5 -f -pi -l test -n test -o tests/parallel/linear-file.mbtiles tests/parallel/in[1234].json tests/parallel/empty[12].json
	./tippecanoe -q -z5 -f -pi -l test -n test -P -o tests/parallel/parallel-file.mbtiles tests/parallel/in[1234].json tests/parallel/empty[12].json
	cat tests/parallel/in[1234].json | ./tippecanoe -q -z5 -f -pi -l test -n test -o tests/parallel/linear-pipe.mbtiles
	cat tests/parallel/in[1234].json | ./tippecanoe -q -z5 -f -pi -l test -n test -P -o tests/parallel/parallel-pipe.mbtiles
	cat tests/parallel/in[1234].json | sed 's/^/@/' | tr '@' '\036' | ./tippecanoe -q -z5 -f -pi -l test -n test -o tests/parallel/implicit-pipe.mbtiles
	./tippecanoe -q -z5 -f -pi -l test -n test -P -o tests/parallel/parallel-pipes.mbtiles <(cat tests/parallel/in1.json) <(cat tests/parallel/empty1.json) <(cat tests/parallel/empty2.json) <(cat tests/parallel/in2.json) /dev/null <(cat tests/parallel/in3.json) <(cat tests/parallel/in4.json)
	./tippecanoe-decode -x generator -x generator_options tests/parallel/linear-file.mbtiles > tests/parallel/linear-file.json
	./tippecanoe-decode -x generator -x generator_options tests/parallel/parallel-file.mbtiles > tests/parallel/parallel-file.json
	./tippecanoe-decode -x generator -x generator_options tests/parallel/linear-pipe.mbtiles > tests/parallel/linear-pipe.json
	./tippecanoe-decode -x generator -x generator_options tests/parallel/parallel-pipe.mbtiles > tests/parallel/parallel-pipe.json
	./tippecanoe-decode -x generator -x generator_options tests/parallel/implicit-pipe.mbtiles > tests/parallel/implicit-pipe.json
	./tippecanoe-decode -x generator -x generator_options tests/parallel/parallel-pipes.mbtiles > tests/parallel/parallel-pipes.json
	cmp tests/parallel/linear-file.json tests/parallel/parallel-file.json
	cmp tests/parallel/linear-file.json tests/parallel/linear-pipe.json
	cmp tests/parallel/linear-file.json tests/parallel/parallel-pipe.json
	cmp tests/parallel/linear-file.json tests/parallel/implicit-pipe.json
	cmp tests/parallel/linear-file.json tests/parallel/parallel-pipes.json
	rm tests/parallel/*.mbtiles tests/parallel/*.json

csv-test: tippecanoe tippecanoe-decode
	# Reading from named CSV
	./tippecanoe -q -zg -f -o tests/csv/out.mbtiles tests/csv/ne_110m_populated_places_simple.csv
	./tippecanoe-decode -x generator -x generator_options tests/csv/out.mbtiles > tests/csv/out.mbtiles.json.check
	cmp tests/csv/out.mbtiles.json.check tests/csv/out.mbtiles.json
	rm -f tests/csv/out.mbtiles.json.check tests/csv/out.mbtiles
	# Reading from named CSV, with nulls
	./tippecanoe -q --empty-csv-columns-are-null -zg -f -o tests/csv/out-null.mbtiles tests/csv/ne_110m_populated_places_simple.csv
	./tippecanoe-decode -x generator tests/csv/out-null.mbtiles > tests/csv/out-null.mbtiles.json.check
	cmp tests/csv/out-null.mbtiles.json.check tests/csv/out-null.mbtiles.json
	rm -f tests/csv/out-null.mbtiles.json.check tests/csv/out-null.mbtiles
	# Same, but specifying csv with -L format
	./tippecanoe -q -zg -f -o tests/csv/out.mbtiles -L'{"file":"", "format":"csv", "layer":"ne_110m_populated_places_simple"}' < tests/csv/ne_110m_populated_places_simple.csv
	./tippecanoe-decode -x generator -x generator_options tests/csv/out.mbtiles > tests/csv/out.mbtiles.json.check
	cmp tests/csv/out.mbtiles.json.check tests/csv/out.mbtiles.json
	rm -f tests/csv/out.mbtiles.json.check tests/csv/out.mbtiles

flatgeobuf-test: tippecanoe tippecanoe-decode
	./tippecanoe -q -f -z0 -l numeric-properties -o tests/flatgeobuf/numeric-properties.mbtiles tests/flatgeobuf/numeric-properties.fgb
	./tippecanoe-decode tests/flatgeobuf/numeric-properties.mbtiles 0 0 0 > tests/flatgeobuf/numeric-properties.json.check
	cmp tests/flatgeobuf/numeric-properties.json.check tests/flatgeobuf/numeric-properties.json
	rm -f tests/flatgeobuf/numeric-properties.mbtiles tests/flatgeobuf/numeric-properties.json.check

layer-json-test: tippecanoe tippecanoe-decode
	# GeoJSON with description and named layer
	./tippecanoe -q -z0 -r1 -yNAME -f -o tests/layer-json/out.mbtiles -L'{"file":"tests/ne_110m_populated_places/in.json", "description":"World cities", "layer":"places"}'
	./tippecanoe-decode -x generator -x generator_options tests/layer-json/out.mbtiles > tests/layer-json/out.mbtiles.json.check
	cmp tests/layer-json/out.mbtiles.json.check tests/layer-json/out.mbtiles.json
	rm -f tests/layer-json/out.mbtiles.json.check tests/layer-json/out.mbtiles
	# Same, but reading from the standard input
	./tippecanoe -q -z0 -r1 -yNAME -f -o tests/layer-json/out.mbtiles -L'{"file":"", "description":"World cities", "layer":"places"}' < tests/ne_110m_populated_places/in.json
	./tippecanoe-decode -x generator -x generator_options tests/layer-json/out.mbtiles > tests/layer-json/out.mbtiles.json.check
	cmp tests/layer-json/out.mbtiles.json.check tests/layer-json/out.mbtiles.json
	rm -f tests/layer-json/out.mbtiles.json.check tests/layer-json/out.mbtiles

raw-tiles-test: tippecanoe tippecanoe-decode tile-join
	./tippecanoe -q -f -e tests/raw-tiles/raw-tiles -r1 -pC tests/raw-tiles/hackspots.geojson
	./tippecanoe-decode -x generator tests/raw-tiles/raw-tiles > tests/raw-tiles/raw-tiles.json.check
	cmp tests/raw-tiles/raw-tiles.json.check tests/raw-tiles/raw-tiles.json
	# Test that -z and -Z work in tippecanoe-decode
	./tippecanoe-decode -x generator -Z6 -z7 tests/raw-tiles/raw-tiles > tests/raw-tiles/raw-tiles-z67.json.check
	cmp tests/raw-tiles/raw-tiles-z67.json.check tests/raw-tiles/raw-tiles-z67.json
	# Test that -z and -Z work in tile-join
	./tile-join -q -f -Z6 -z7 -e tests/raw-tiles/raw-tiles-z67 tests/raw-tiles/raw-tiles
	./tippecanoe-decode -x generator tests/raw-tiles/raw-tiles-z67 > tests/raw-tiles/raw-tiles-z67-join.json.check
	cmp tests/raw-tiles/raw-tiles-z67-join.json.check tests/raw-tiles/raw-tiles-z67-join.json
	rm -rf tests/raw-tiles/raw-tiles tests/raw-tiles/raw-tiles-z67 tests/raw-tiles/raw-tiles.json.check raw-tiles-z67.json.check tests/raw-tiles/raw-tiles-z67-join.json.check
	# Test that metadata.json is created even if all features are clipped away
	./tippecanoe -q -f -e tests/raw-tiles/nothing tests/raw-tiles/nothing.geojson
	./tippecanoe-decode -x generator tests/raw-tiles/nothing > tests/raw-tiles/nothing.json.check
	cmp tests/raw-tiles/nothing.json.check tests/raw-tiles/nothing.json
	rm -r tests/raw-tiles/nothing tests/raw-tiles/nothing.json.check
	# Test that a non-string value in metadata.json is reported and skipped
	# instead of being read as a string (which used to crash)
	./tippecanoe -q -f -e tests/raw-tiles/nonstring tests/raw-tiles/hackspots.geojson
	sed -i.bak 's/"minzoom": "0"/"minzoom": 0/' tests/raw-tiles/nonstring/metadata.json
	rm tests/raw-tiles/nonstring/metadata.json.bak
	grep -q '"minzoom": 0' tests/raw-tiles/nonstring/metadata.json
	./tippecanoe-decode -x generator tests/raw-tiles/nonstring > /dev/null
	rm -r tests/raw-tiles/nonstring

pmtiles-test: tippecanoe tippecanoe-decode tile-join
	./tippecanoe -q -f -o tests/pmtiles/hackspots.pmtiles -r1 -pC tests/raw-tiles/hackspots.geojson
	./tippecanoe-decode -x generator tests/pmtiles/hackspots.pmtiles > tests/pmtiles/hackspots.json.check
	cmp tests/pmtiles/hackspots.json.check tests/pmtiles/hackspots.json
	# Test generating pmtiles first and then converting to mbtiles with tile-join.
	./tile-join -q -f -pC -o tests/pmtiles/joined.mbtiles tests/pmtiles/hackspots.pmtiles
	./tippecanoe-decode -x generator tests/pmtiles/joined.mbtiles > tests/pmtiles/joined.json.check
	cmp tests/pmtiles/joined.json.check tests/pmtiles/joined.json
	rm -r tests/pmtiles/hackspots.json.check tests/pmtiles/hackspots.pmtiles

enumerate-test: tippecanoe tippecanoe-enumerate
	./tippecanoe -q -z5 -f -o tests/ne_110m_admin_0_countries/out/enum.mbtiles tests/ne_110m_admin_0_countries/in.json.gz
	./tippecanoe-enumerate tests/ne_110m_admin_0_countries/out/enum.mbtiles > tests/ne_110m_admin_0_countries/out/enum.check
	cmp tests/ne_110m_admin_0_countries/out/enum.check tests/ne_110m_admin_0_countries/out/enum
	rm tests/ne_110m_admin_0_countries/out/enum.mbtiles tests/ne_110m_admin_0_countries/out/enum.check

join-test: tippecanoe tippecanoe-decode tile-join
	./tippecanoe -q -f -z12 -o tests/join-population/tabblock_06001420.mbtiles -YALAND10:'Land area' -L'{"file": "tests/join-population/tabblock_06001420.json", "description": "population"}'
	./tippecanoe -q -f -Z5 -z10 -o tests/join-population/macarthur.mbtiles -l macarthur tests/join-population/macarthur.json
	./tile-join -q -f -Z6 -z9 -o tests/join-population/macarthur-6-9.mbtiles tests/join-population/macarthur.mbtiles
	./tippecanoe-decode -x generator tests/join-population/macarthur-6-9.mbtiles > tests/join-population/macarthur-6-9.mbtiles.json.check
	cmp tests/join-population/macarthur-6-9.mbtiles.json.check tests/join-population/macarthur-6-9.mbtiles.json
	./tile-join -q -f -Z6 -z9 -X -o tests/join-population/macarthur-6-9-exclude.mbtiles tests/join-population/macarthur.mbtiles
	./tippecanoe-decode -x generator tests/join-population/macarthur-6-9-exclude.mbtiles > tests/join-population/macarthur-6-9-exclude.mbtiles.json.check
	cmp tests/join-population/macarthur-6-9-exclude.mbtiles.json.check tests/join-population/macarthur-6-9-exclude.mbtiles.json
	rm -f tests/join-population/macarthur-6-9.mbtiles.json.check tests/join-population/macarthur-6-9.mbtiles tests/join-population/macarthur-6-9-exclude.mbtiles.json.check tests/join-population/macarthur-6-9-exclude.mbtiles
	./tippecanoe -q -f -d10 -D10 -Z9 -z11 -o tests/join-population/macarthur2.mbtiles -l macarthur tests/join-population/macarthur2.json
	./tile-join --quiet --force -o tests/join-population/joined.mbtiles -x GEOID10 -c tests/join-population/population.csv tests/join-population/tabblock_06001420.mbtiles
	./tile-join --quiet --force -o tests/join-population/joined-null.mbtiles --empty-csv-columns-are-null -x GEOID10 -c tests/join-population/population.csv tests/join-population/tabblock_06001420.mbtiles
	./tile-join --quiet --force --no-tile-stats -o tests/join-population/joined-no-tile-stats.mbtiles -x GEOID10 -c tests/join-population/population.csv tests/join-population/tabblock_06001420.mbtiles
	./tile-join --quiet --force --tile-stats-attributes-limit=1 -o tests/join-population/joined-tile-stats-attributes-limit.mbtiles -x GEOID10 -c tests/join-population/population.csv tests/join-population/tabblock_06001420.mbtiles
	./tile-join --quiet --force --tile-stats-sample-values-limit=1 -o tests/join-population/joined-tile-stats-sample-values-limit.mbtiles -x GEOID10 -c tests/join-population/population.csv tests/join-population/tabblock_06001420.mbtiles
	./tile-join --quiet --force --tile-stats-values-limit=1 -o tests/join-population/joined-tile-stats-values-limit.mbtiles -x GEOID10 -c tests/join-population/population.csv tests/join-population/tabblock_06001420.mbtiles
	./tile-join -q -f -i -o tests/join-population/joined-i.mbtiles -x GEOID10 -c tests/join-population/population.csv tests/join-population/tabblock_06001420.mbtiles
	./tile-join -q -f -o tests/join-population/merged.mbtiles tests/join-population/tabblock_06001420.mbtiles tests/join-population/macarthur.mbtiles tests/join-population/macarthur2.mbtiles
	./tile-join -q -f -c tests/join-population/windows.csv -o tests/join-population/windows.mbtiles tests/join-population/macarthur.mbtiles
	./tippecanoe-decode -x generator --maximum-zoom=11 --minimum-zoom=4 tests/join-population/joined.mbtiles > tests/join-population/joined.mbtiles.json.check
	./tippecanoe-decode -x generator --maximum-zoom=11 --minimum-zoom=4 tests/join-population/joined-null.mbtiles > tests/join-population/joined-null.mbtiles.json.check
	./tippecanoe-decode -x generator --maximum-zoom=11 --minimum-zoom=4 tests/join-population/joined-no-tile-stats.mbtiles > tests/join-population/joined-no-tile-stats.mbtiles.json.check
	./tippecanoe-decode -x generator --maximum-zoom=11 --minimum-zoom=4 tests/join-population/joined-tile-stats-attributes-limit.mbtiles > tests/join-population/joined-tile-stats-attributes-limit.mbtiles.json.check
	./tippecanoe-decode -x generator --maximum-zoom=11 --minimum-zoom=4 tests/join-population/joined-tile-stats-values-limit.mbtiles > tests/join-population/joined-tile-stats-values-limit.mbtiles.json.check
	./tippecanoe-decode -x generator --maximum-zoom=11 --minimum-zoom=4 tests/join-population/joined-tile-stats-sample-values-limit.mbtiles > tests/join-population/joined-tile-stats-sample-values-limit.mbtiles.json.check
	./tippecanoe-decode -x generator tests/join-population/joined-i.mbtiles > tests/join-population/joined-i.mbtiles.json.check
	./tippecanoe-decode -x generator tests/join-population/merged.mbtiles > tests/join-population/merged.mbtiles.json.check
	./tippecanoe-decode -x generator tests/join-population/windows.mbtiles > tests/join-population/windows.mbtiles.json.check
	cmp tests/join-population/joined.mbtiles.json.check tests/join-population/joined.mbtiles.json
	cmp tests/join-population/joined-null.mbtiles.json.check tests/join-population/joined-null.mbtiles.json
	cmp tests/join-population/joined-no-tile-stats.mbtiles.json.check tests/join-population/joined-no-tile-stats.mbtiles.json
	cmp tests/join-population/joined-tile-stats-attributes-limit.mbtiles.json.check tests/join-population/joined-tile-stats-attributes-limit.mbtiles.json
	cmp tests/join-population/joined-tile-stats-sample-values-limit.mbtiles.json.check tests/join-population/joined-tile-stats-sample-values-limit.mbtiles.json
	cmp tests/join-population/joined-tile-stats-values-limit.mbtiles.json.check tests/join-population/joined-tile-stats-values-limit.mbtiles.json
	cmp tests/join-population/joined-i.mbtiles.json.check tests/join-population/joined-i.mbtiles.json
	cmp tests/join-population/merged.mbtiles.json.check tests/join-population/merged.mbtiles.json
	cmp tests/join-population/windows.mbtiles.json.check tests/join-population/windows.mbtiles.json
	rm -f tests/join-population/joined-null.mbtiles tests/join-population/joined-null.mbtiles.json.check
	./tile-join -q -f -l macarthur -n "macarthur name" -N "macarthur description" -A "macarthur's attribution" -o tests/join-population/just-macarthur.mbtiles tests/join-population/merged.mbtiles
	./tile-join -q -f -L macarthur -o tests/join-population/no-macarthur.mbtiles tests/join-population/merged.mbtiles
	./tippecanoe-decode -x generator tests/join-population/just-macarthur.mbtiles > tests/join-population/just-macarthur.mbtiles.json.check
	./tippecanoe-decode -x generator tests/join-population/no-macarthur.mbtiles > tests/join-population/no-macarthur.mbtiles.json.check
	cmp tests/join-population/just-macarthur.mbtiles.json.check tests/join-population/just-macarthur.mbtiles.json
	cmp tests/join-population/no-macarthur.mbtiles.json.check tests/join-population/no-macarthur.mbtiles.json
	./tile-join -q --no-tile-compression -f -e tests/join-population/raw-merged-folder tests/join-population/tabblock_06001420.mbtiles tests/join-population/macarthur.mbtiles tests/join-population/macarthur2.mbtiles
	./tippecanoe-decode -x generator tests/join-population/raw-merged-folder > tests/join-population/raw-merged-folder.json.check
	cmp tests/join-population/raw-merged-folder.json.check tests/join-population/raw-merged-folder.json
	rm -f tests/join-population/raw-merged-folder.json.check
	./tippecanoe -q -z12 -f -e tests/join-population/tabblock_06001420-folder -YALAND10:'Land area' -L'{"file": "tests/join-population/tabblock_06001420.json", "description": "population"}'
	./tippecanoe -q -Z5 -z10 -f -e tests/join-population/macarthur-folder -l macarthur tests/join-population/macarthur.json
	./tippecanoe -q -d10 -D10 -Z9 -z11 -f -e tests/join-population/macarthur2-folder -l macarthur tests/join-population/macarthur2.json
	./tile-join -q -f -o tests/join-population/merged-folder.mbtiles tests/join-population/tabblock_06001420-folder tests/join-population/macarthur-folder tests/join-population/macarthur2-folder
	./tippecanoe-decode -x generator tests/join-population/merged-folder.mbtiles > tests/join-population/merged-folder.mbtiles.json.check
	cmp tests/join-population/merged-folder.mbtiles.json.check tests/join-population/merged-folder.mbtiles.json
	./tile-join -q -n "merged name" -N "merged description" -f -e tests/join-population/merged-mbtiles-to-folder tests/join-population/tabblock_06001420.mbtiles tests/join-population/macarthur.mbtiles tests/join-population/macarthur2.mbtiles
	./tile-join -q -n "merged name" -N "merged description" -f -e tests/join-population/merged-folders-to-folder tests/join-population/tabblock_06001420-folder tests/join-population/macarthur-folder tests/join-population/macarthur2-folder
	./tippecanoe-decode -x generator -x generator_options tests/join-population/merged-mbtiles-to-folder > tests/join-population/merged-mbtiles-to-folder.json.check
	./tippecanoe-decode -x generator -x generator_options tests/join-population/merged-folders-to-folder > tests/join-population/merged-folders-to-folder.json.check
	cmp tests/join-population/merged-mbtiles-to-folder.json.check tests/join-population/merged-folders-to-folder.json.check
	rm -f tests/join-population/merged-mbtiles-to-folder.json.check tests/join-population/merged-folders-to-folder.json.check
	./tile-join -q -f -c tests/join-population/windows.csv -o tests/join-population/windows-merged.mbtiles tests/join-population/macarthur.mbtiles tests/join-population/macarthur2-folder
	./tile-join -q -c tests/join-population/windows.csv -f -e tests/join-population/windows-merged-folder tests/join-population/macarthur.mbtiles tests/join-population/macarthur2-folder
	./tile-join -q -f -o tests/join-population/windows-merged2.mbtiles tests/join-population/windows-merged-folder
	./tippecanoe-decode -x generator -x generator_options tests/join-population/windows-merged.mbtiles > tests/join-population/windows-merged.mbtiles.json.check
	./tippecanoe-decode -x generator -x generator_options tests/join-population/windows-merged2.mbtiles > tests/join-population/windows-merged2.mbtiles.json.check
	cmp tests/join-population/windows-merged.mbtiles.json.check tests/join-population/windows-merged2.mbtiles.json.check
	./tile-join -q -f -o tests/join-population/macarthur-and-macarthur2-merged.mbtiles tests/join-population/macarthur.mbtiles tests/join-population/macarthur2-folder
	./tile-join -q -f -e tests/join-population/macarthur-and-macarthur2-folder tests/join-population/macarthur.mbtiles tests/join-population/macarthur2-folder
	./tile-join -q -f -o tests/join-population/macarthur-and-macarthur2-merged2.mbtiles tests/join-population/macarthur-and-macarthur2-folder
	./tippecanoe-decode -x generator -x generator_options tests/join-population/macarthur-and-macarthur2-merged.mbtiles > tests/join-population/macarthur-and-macarthur2-merged.mbtiles.json.check
	./tippecanoe-decode -x generator -x generator_options tests/join-population/macarthur-and-macarthur2-merged2.mbtiles > tests/join-population/macarthur-and-macarthur2-merged2.mbtiles.json.check
	cmp tests/join-population/macarthur-and-macarthur2-merged.mbtiles.json.check tests/join-population/macarthur-and-macarthur2-merged2.mbtiles.json.check
	rm tests/join-population/tabblock_06001420.mbtiles tests/join-population/joined.mbtiles tests/join-population/joined-i.mbtiles tests/join-population/joined.mbtiles.json.check tests/join-population/joined-i.mbtiles.json.check tests/join-population/macarthur.mbtiles tests/join-population/merged.mbtiles tests/join-population/merged.mbtiles.json.check  tests/join-population/merged-folder.mbtiles tests/join-population/macarthur2.mbtiles tests/join-population/windows.mbtiles tests/join-population/windows-merged.mbtiles tests/join-population/windows-merged2.mbtiles tests/join-population/windows.mbtiles.json.check tests/join-population/just-macarthur.mbtiles tests/join-population/no-macarthur.mbtiles tests/join-population/just-macarthur.mbtiles.json.check tests/join-population/no-macarthur.mbtiles.json.check tests/join-population/merged-folder.mbtiles.json.check tests/join-population/windows-merged.mbtiles.json.check tests/join-population/windows-merged2.mbtiles.json.check tests/join-population/macarthur-and-macarthur2-merged.mbtiles tests/join-population/macarthur-and-macarthur2-merged2.mbtiles tests/join-population/macarthur-and-macarthur2-merged.mbtiles.json.check tests/join-population/macarthur-and-macarthur2-merged2.mbtiles.json.check
	rm -rf tests/join-population/raw-merged-folder tests/join-population/tabblock_06001420-folder tests/join-population/macarthur-folder tests/join-population/macarthur2-folder tests/join-population/merged-mbtiles-to-folder tests/join-population/merged-folders-to-folder tests/join-population/windows-merged-folder tests/join-population/macarthur-and-macarthur2-folder
	# Test renaming of layers
	./tippecanoe -q -f -Z5 -z10 -o tests/join-population/macarthur.mbtiles -l macarthur1 tests/join-population/macarthur.json
	./tippecanoe -q -f -Z5 -z10 -o tests/join-population/macarthur2.mbtiles -l macarthur2 tests/join-population/macarthur2.json
	./tile-join -q -R macarthur1:one --rename-layer=macarthur2:two -f -o tests/join-population/renamed.mbtiles tests/join-population/macarthur.mbtiles tests/join-population/macarthur2.mbtiles
	./tippecanoe-decode -x generator tests/join-population/renamed.mbtiles > tests/join-population/renamed.mbtiles.json.check
	cmp tests/join-population/renamed.mbtiles.json.check tests/join-population/renamed.mbtiles.json
	rm -f tests/join-population/renamed.mbtiles.json.check tests/join-population/renamed.mbtiles.json.check tests/join-population/macarthur.mbtiles tests/join-population/macarthur2.mbtiles
	# Make sure the concatenated name isn't too long
	./tippecanoe -q -f -z0 -n 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ' -o tests/join-population/macarthur.mbtiles tests/join-population/macarthur.json
	./tile-join -f -o tests/join-population/concat.mbtiles tests/join-population/macarthur.mbtiles tests/join-population/macarthur.mbtiles tests/join-population/macarthur.mbtiles tests/join-population/macarthur.mbtiles tests/join-population/macarthur.mbtiles tests/join-population/macarthur.mbtiles
	./tippecanoe-decode -x generator tests/join-population/concat.mbtiles > tests/join-population/concat.mbtiles.json.check
	cmp tests/join-population/concat.mbtiles.json.check tests/join-population/concat.mbtiles.json
	rm tests/join-population/concat.mbtiles.json.check tests/join-population/concat.mbtiles tests/join-population/macarthur.mbtiles
	# Test reading list of input files from file
	./tippecanoe -q -f -Z5 -z10 -o tests/readfile/macarthur.mbtiles -l macarthur1 tests/join-population/macarthur.json
	./tippecanoe -q -f -Z5 -z10 -o tests/readfile/macarthur2.mbtiles -l macarthur2 tests/join-population/macarthur2.json
	./tile-join -q -R macarthur1:one --rename-layer=macarthur2:two -f -o tests/readfile/renamed.mbtiles tests/readfile/macarthur.mbtiles tests/readfile/macarthur2.mbtiles
	./tippecanoe-decode -x generator -x generator_options tests/readfile/renamed.mbtiles > tests/readfile/renamed.mbtiles.json.check
	./tile-join -q -R macarthur1:one --rename-layer=macarthur2:two -f -r tests/readfile/readfile.list -o tests/readfile/readfile.mbtiles
	./tippecanoe-decode -x generator -x generator_options tests/readfile/readfile.mbtiles > tests/readfile/readfile.mbtiles.json.check
	cmp tests/readfile/renamed.mbtiles.json.check tests/readfile/readfile.mbtiles.json.check
	rm tests/readfile/renamed.mbtiles.json.check tests/readfile/readfile.mbtiles.json.check  tests/readfile/readfile.mbtiles tests/readfile/renamed.mbtiles
	#`
	# Make sure empty tilesets work
	#
	# mbtiles:
	./tippecanoe -q -z0 -f -o tests/join-population/empty.mbtiles tests/join-population/empty.json
	./tile-join -f -o tests/join-population/empty.out.mbtiles tests/join-population/empty.mbtiles
	./tippecanoe-decode -x generator -x generator_options -x name -x description tests/join-population/empty.mbtiles > tests/join-population/empty.out.json.check
	cmp tests/join-population/empty.out.json.check tests/join-population/empty.out.json
	rm -f tests/join-population/empty.mbtiles tests/join-population/empty.out.mbtiles tests/join-population/empty.out.json.check
	# pmtiles:
	./tippecanoe -q -z0 -f -o tests/join-population/empty.pmtiles tests/join-population/empty.json
	./tile-join -f -o tests/join-population/empty.out.pmtiles tests/join-population/empty.pmtiles
	./tippecanoe-decode -x generator -x generator_options -x name -x description tests/join-population/empty.pmtiles > tests/join-population/empty.out.json.check
	cmp tests/join-population/empty.out.json.check tests/join-population/empty.out.json
	rm -f tests/join-population/empty.pmtiles tests/join-population/empty.out.pmtiles tests/join-population/empty.out.json.check
	# pmtiles again, with --overzoom
	./tippecanoe -q -z0 -f -o tests/join-population/empty.pmtiles tests/join-population/empty.json
	./tile-join --overzoom -f -o tests/join-population/empty.out.pmtiles tests/join-population/empty.pmtiles
	./tippecanoe-decode -x generator -x generator_options -x name -x description tests/join-population/empty.pmtiles > tests/join-population/empty.out.json.check
	cmp tests/join-population/empty.out.json.check tests/join-population/empty.out.json
	rm -f tests/join-population/empty.pmtiles tests/join-population/empty.out.pmtiles tests/join-population/empty.out.json.check
	# dirtiles:
	./tippecanoe -q -z0 -f -e tests/join-population/empty.dirtiles tests/join-population/empty.json
	./tile-join -f -e tests/join-population/empty.out.dirtiles tests/join-population/empty.dirtiles
	./tippecanoe-decode -x generator -x generator_options -x name -x description tests/join-population/empty.dirtiles > tests/join-population/empty.out.json.check
	cmp tests/join-population/empty.out.json.check tests/join-population/empty.out.json
	rm -rf tests/join-population/empty.dirtiles tests/join-population/empty.out.dirtiles tests/join-population/empty.out.json.check
	#
	# Test overzooming of tilesets with different maxzooms
	#
	mkdir -p tests/ne_110m_ocean/join
	./tippecanoe -q -z2 -f -o tests/ne_110m_ocean/join/ocean.mbtiles tests/ne_110m_ocean/in.json
	./tippecanoe -q -z4 -d8 -y name -f -o tests/ne_110m_ocean/join/countries.mbtiles tests/ne_110m_admin_0_countries/in.json.gz
	./tile-join --overzoom -f -o tests/ne_110m_ocean/join/joined.mbtiles tests/ne_110m_ocean/join/ocean.mbtiles tests/ne_110m_ocean/join/countries.mbtiles
	./tippecanoe-decode -x generator tests/ne_110m_ocean/join/joined.mbtiles > tests/ne_110m_ocean/join/joined.mbtiles.json.check
	cmp tests/ne_110m_ocean/join/joined.mbtiles.json.check tests/ne_110m_ocean/join/joined.mbtiles.json
	rm -f tests/ne_110m_ocean/join/ocean.mbtiles tests/ne_110m_ocean/join/countries.mbtiles tests/ne_110m_ocean/join/joined.mbtiles tests/ne_110m_ocean/join/joined.mbtiles.json.check

join-filter-test: tippecanoe tippecanoe-decode tile-join
	# Comes out different from the direct tippecanoe run because null attributes are lost
	./tippecanoe -q -z0 -f -o tests/feature-filter/out/all.mbtiles tests/feature-filter/in.json
	./tile-join -q -J tests/feature-filter/filter -f -o tests/feature-filter/out/filtered.mbtiles tests/feature-filter/out/all.mbtiles
	./tippecanoe-decode -x generator tests/feature-filter/out/filtered.mbtiles > tests/feature-filter/out/filtered.json.check
	cmp tests/feature-filter/out/filtered.json.check tests/feature-filter/out/filtered.json.standard
	rm -f tests/feature-filter/out/filtered.json.check tests/feature-filter/out/filtered.mbtiles tests/feature-filter/out/all.mbtiles
	# Test zoom level filtering
	./tippecanoe -q -r1 -z8 -f -o tests/feature-filter/out/places.mbtiles tests/ne_110m_populated_places/in.json
	./tile-join -q -J tests/feature-filter/places-filter -f -o tests/feature-filter/out/places-filter.mbtiles tests/feature-filter/out/places.mbtiles
	./tippecanoe-decode -x generator tests/feature-filter/out/places-filter.mbtiles > tests/feature-filter/out/places-filter.mbtiles.json.check
	cmp tests/feature-filter/out/places-filter.mbtiles.json.check tests/feature-filter/out/places-filter.mbtiles.json.standard
	rm -f tests/feature-filter/out/places.mbtiles tests/feature-filter/out/places-filter.mbtiles tests/feature-filter/out/places-filter.mbtiles.json.check

# Use this target to regenerate the standards that the tests are compared against
# after making a change that legitimately changes their output


# Don't test overflow with geobuf, because it fails (https://github.com/mapbox/geobuf/issues/87)
# Don't test stringids with geobuf, because it fails
nogeobuf = tests/overflow/out/-z0.json $(wildcard tests/stringid/out/*.json)
# tippecanoe-rs makes the same tiles as manby from these Geobuf inputs, and
# neither matches the expected output
nogeobuf += tests/coalesce-id/out/-z1_--coalesce_--reorder.json
nogeobuf += tests/pointlm/out/-z15_-Z15_--drop-smallest-as-needed_-M300.json
nogeobuf += tests/single-polygons/out/-Z21_-zg_-D10_-d10.json
# The -at size estimate counts the bytes of the temporary files, which are
# not the same when the attributes come from Geobuf; here too tippecanoe-rs
# makes the same tiles as manby
nogeobuf += tests/tl_2022_11_tract/out/-z14_-Z12_--coalesce-densest-as-needed_--generate-variable-depth-tile-pyramid_-M25000.json
nogeobuf += tests/tl_2022_11_tract/out/-z17_-Z10_--generate-variable-depth-tile-pyramid_--drop-by-attribute-as-needed%3dALAND_-M15000.json
nogeobuf += tests/tl_2022_11_tract/out/-z17_-Z10_--coalesce-densest-as-needed_--generate-variable-depth-tile-pyramid_-M15000.json
nogeobuf += tests/tl_2022_11_tract/out/-z17_-Z12_--coalesce-densest-as-needed_--generate-variable-depth-tile-pyramid_-M10000.json
geobuf-test: tippecanoe-json-tool $(addsuffix .checkbuf,$(filter-out $(nogeobuf),$(TESTS)))

# Run one at a time: the recipes rewrite the .geobuf files of their directory.
%.json.checkbuf:
	for i in $(wildcard $(subst $(SPACE),/,$(wordlist 1,2,$(subst /, ,$@)))/*.json); do ./tippecanoe-json-tool -w $$i | ./json2geobuf > $$i.geobuf; done
	for i in $(wildcard $(subst $(SPACE),/,$(wordlist 1,2,$(subst /, ,$@)))/*.json.gz); do gzip -dc $$i | ./tippecanoe-json-tool -w | ./json2geobuf > $$i.geobuf; done
	./tippecanoe -q -a@ -f -o $@.mbtiles $(call testargs,$(patsubst %.json.checkbuf,%,$(word 4,$(subst /, ,$@)))) $(foreach suffix,$(suffixes),$(addsuffix .geobuf,$(sort $(wildcard $(subst $(SPACE),/,$(wordlist 1,2,$(subst /, ,$@)))/*.$(suffix))))) < /dev/null
	./tippecanoe-decode -x generator $@.mbtiles | sed 's/checkbuf/check/g' | sed 's/\.geobuf//g' > $@.out
	cmp $@.out $(patsubst %.checkbuf,%,$@)
	rm $@.out $@.mbtiles

pbf-test: tippecanoe-decode
	./tippecanoe-decode -x generator tests/pbf/11-328-791.vector.pbf 11 328 791 > tests/pbf/11-328-791.vector.pbf.out
	cmp tests/pbf/11-328-791.json tests/pbf/11-328-791.vector.pbf.out
	rm tests/pbf/11-328-791.vector.pbf.out
	./tippecanoe-decode -x generator -s EPSG:3857 tests/pbf/11-328-791.vector.pbf 11 328 791 > tests/pbf/11-328-791.3857.vector.pbf.out
	cmp tests/pbf/11-328-791.3857.json tests/pbf/11-328-791.3857.vector.pbf.out
	rm tests/pbf/11-328-791.3857.vector.pbf.out

overzoom-test: tippecanoe-overzoom
	# Basic operation
	./tippecanoe-overzoom -o tests/pbf/13-1310-3166.pbf tests/pbf/11-327-791.pbf 11/327/791 13/1310/3166
	./tippecanoe-decode tests/pbf/13-1310-3166.pbf 13 1310 3166 > tests/pbf/13-1310-3166.pbf.json.check
	cmp tests/pbf/13-1310-3166.pbf.json.check tests/pbf/13-1310-3166.pbf.json
	rm tests/pbf/13-1310-3166.pbf tests/pbf/13-1310-3166.pbf.json.check
	# Make sure feature order is stable
	# Large -b20 tile buffer to prevent altering the geometries by clipping
	./tippecanoe-overzoom -b20 --preserve-input-order -o tests/pbf/11-327-791-out.pbf tests/pbf/11-327-791.pbf 11/327/791 11/327/791
	./tippecanoe-decode tests/pbf/11-327-791.pbf 11 327 791 > tests/pbf/11-327-791.json
	./tippecanoe-decode tests/pbf/11-327-791-out.pbf 11 327 791 > tests/pbf/11-327-791-out.json
	cmp tests/pbf/11-327-791.json tests/pbf/11-327-791-out.json
	rm tests/pbf/11-327-791.json tests/pbf/11-327-791-out.json tests/pbf/11-327-791-out.pbf
	# Basic operation, multiple input form
	./tippecanoe-overzoom -o tests/pbf/13-1310-3166.pbf -t 13/1310/3166 tests/pbf/11-327-791.pbf 11/327/791
	./tippecanoe-decode tests/pbf/13-1310-3166.pbf 13 1310 3166 > tests/pbf/13-1310-3166.pbf.json.check
	cmp tests/pbf/13-1310-3166.pbf.json.check tests/pbf/13-1310-3166.pbf.json
	rm tests/pbf/13-1310-3166.pbf tests/pbf/13-1310-3166.pbf.json.check
	# Multiple inputs, no compression
	./tippecanoe-overzoom --no-tile-compression -o tests/pbf/13-1310-3166-ne.pbf -t 13/1310/3166 tests/pbf/11-327-791.pbf 11/327/791 tests/pbf/0-0-0.pbf 0/0/0
	./tippecanoe-decode tests/pbf/13-1310-3166-ne.pbf 13 1310 3166 > tests/pbf/13-1310-3166-ne.pbf.json.check
	cmp tests/pbf/13-1310-3166-ne.pbf.json.check tests/pbf/13-1310-3166-ne.pbf.json
	rm tests/pbf/13-1310-3166-ne.pbf tests/pbf/13-1310-3166-ne.pbf.json.check
	# Different detail and buffer, and attribute stripping
	./tippecanoe-overzoom -d8 -b30 -y NAME -y name -y scalerank -o tests/pbf/13-1310-3166-8-30.pbf tests/pbf/11-327-791.pbf 11/327/791 13/1310/3166
	./tippecanoe-decode tests/pbf/13-1310-3166-8-30.pbf 13 1310 3166 > tests/pbf/13-1310-3166-8-30.pbf.json.check
	cmp tests/pbf/13-1310-3166-8-30.pbf.json.check tests/pbf/13-1310-3166-8-30.pbf.json
	rm tests/pbf/13-1310-3166-8-30.pbf tests/pbf/13-1310-3166-8-30.pbf.json.check
	# No features in child tile
	./tippecanoe-overzoom -o tests/pbf/14-2616-6331.pbf tests/pbf/11-327-791.pbf 11/327/791 14/2616/6331
	cmp tests/pbf/14-2616-6331.pbf /dev/null
	rm tests/pbf/14-2616-6331.pbf
	# Thinning
	# 243 features in the source tile tests/pbf/0-0-0-pop.pbf
	# 9 of them survive as the best of each cluster of 30
	# ./tippecanoe -z1 -r30 --retain-points-multiplier 30 -f -e out.dir tests/ne_110m_populated_places/in.json
	# cp out.dir/0/0/0.pbf tests/pbf/0-0-0-pop.pbf
	./tippecanoe-overzoom -y NAME -m -o tests/pbf/0-0-0-pop-filtered.pbf tests/pbf/0-0-0-pop.pbf 0/0/0 0/0/0
	./tippecanoe-decode tests/pbf/0-0-0-pop-filtered.pbf 0 0 0 > tests/pbf/0-0-0-pop-filtered.pbf.json.check
	cmp tests/pbf/0-0-0-pop-filtered.pbf.json.check tests/pbf/0-0-0-pop-filtered.pbf.json
	rm tests/pbf/0-0-0-pop-filtered.pbf tests/pbf/0-0-0-pop-filtered.pbf.json.check
	# Thinning with accumulation
	./tippecanoe-overzoom -y NAME -m --accumulate-attribute NAME:comma -o tests/pbf/0-0-0-pop-accum.pbf tests/pbf/0-0-0-pop.pbf 0/0/0 0/0/0
	./tippecanoe-decode tests/pbf/0-0-0-pop-accum.pbf 0 0 0 > tests/pbf/0-0-0-pop-accum.pbf.json.check
	cmp tests/pbf/0-0-0-pop-accum.pbf.json.check tests/pbf/0-0-0-pop-accum.pbf.json
	rm tests/pbf/0-0-0-pop-accum.pbf tests/pbf/0-0-0-pop-accum.pbf.json.check
	# Filtering
	# 243 features in the source tile tests/pbf/0-0-0-pop.pbf
	# 27 of them match the filter and are retained
	./tippecanoe-overzoom -y NAME -j'{"*":["==","SCALERANK",0]}' -o tests/pbf/0-0-0-pop-expr.pbf tests/pbf/0-0-0-pop.pbf 0/0/0 0/0/0
	./tippecanoe-decode tests/pbf/0-0-0-pop-expr.pbf 0 0 0 > tests/pbf/0-0-0-pop-expr.pbf.json.check
	cmp tests/pbf/0-0-0-pop-expr.pbf.json.check tests/pbf/0-0-0-pop-expr.pbf.json
	rm tests/pbf/0-0-0-pop-expr.pbf tests/pbf/0-0-0-pop-expr.pbf.json.check
	# Same filter test, but reading the filter from a file
	./tippecanoe-overzoom -y NAME -J tests/pbf/scalerank-0-filter.json -o tests/pbf/0-0-0-pop-expr.pbf tests/pbf/0-0-0-pop.pbf 0/0/0 0/0/0
	./tippecanoe-decode tests/pbf/0-0-0-pop-expr.pbf 0 0 0 > tests/pbf/0-0-0-pop-expr.pbf.json.check
	cmp tests/pbf/0-0-0-pop-expr.pbf.json.check tests/pbf/0-0-0-pop-expr.pbf.json
	rm tests/pbf/0-0-0-pop-expr.pbf tests/pbf/0-0-0-pop-expr.pbf.json.check
	# Filtering with multiplier
	# 243 features in the source tile tests/pbf/0-0-0-pop.pbf
	# 8 features survive into the output, from 9 clusters of 30
	./tippecanoe-overzoom -y NAME -y SCALERANK -j'{"*":["==","SCALERANK",0]}' -m -o tests/pbf/0-0-0-filter-mult.pbf tests/pbf/0-0-0-pop.pbf 0/0/0 0/0/0
	./tippecanoe-decode tests/pbf/0-0-0-filter-mult.pbf 0 0 0 > tests/pbf/0-0-0-filter-mult.pbf.json.check
	cmp tests/pbf/0-0-0-filter-mult.pbf.json.check tests/pbf/0-0-0-filter-mult.pbf.json
	rm tests/pbf/0-0-0-filter-mult.pbf tests/pbf/0-0-0-filter-mult.pbf.json.check
	# Filtering with multiplier and preserve-input-order
	# 243 features in the source tile tests/pbf/0-0-0-pop.pbf
	./tippecanoe-overzoom -y NAME -y SCALERANK -y LATITUDE -j'{"*":[">","LATITUDE",40]}' -m --preserve-input-order -o tests/pbf/0-0-0-filter-mult-order.pbf tests/pbf/0-0-0-pop.pbf 0/0/0 0/0/0
	./tippecanoe-decode tests/pbf/0-0-0-filter-mult-order.pbf 0 0 0 > tests/pbf/0-0-0-filter-mult-order.pbf.json.check
	cmp tests/pbf/0-0-0-filter-mult-order.pbf.json.check tests/pbf/0-0-0-filter-mult-order.pbf.json
	rm tests/pbf/0-0-0-filter-mult-order.pbf tests/pbf/0-0-0-filter-mult-order.pbf.json.check
	# Test that overzooming with a multiplier exactly reverses the effect of tiling with a multiplier
	./tippecanoe -q -z5 --preserve-point-density-threshold 8 --retain-points-multiplier 3 -f -e tests/muni/out/out.dir tests/muni/muni.json
	./tippecanoe -q -z5 --preserve-point-density-threshold 8 -f -o tests/muni/out/out.mbtiles tests/muni/muni.json
	./tippecanoe-overzoom -m -o tests/muni/out/out.dir/000.pbf tests/muni/out/out.dir/0/0/0.pbf 0/0/0 0/0/0
	./tippecanoe-decode tests/muni/out/out.mbtiles 0 0 0 > tests/muni/out/out.dir/direct.json
	./tippecanoe-decode tests/muni/out/out.dir/000.pbf 0 0 0 > tests/muni/out/out.dir/overzoomed.json
	cmp tests/muni/out/out.dir/overzoomed.json tests/muni/out/out.dir/direct.json
	rm -rf tests/muni/out/out.dir tests/muni/out/out.mbtiles tests/muni/out/out.dir/overzoomed.json tests/muni/out/out.dir/direct.json
	# Tiny polygon reduction
	./tippecanoe-overzoom --line-simplification=5 --tiny-polygon-size=50 -o tests/pbf/countries-0-0-0.pbf.out tests/pbf/countries-0-0-0.pbf 0/0/0 0/0/0
	./tippecanoe-decode tests/pbf/countries-0-0-0.pbf.out 0 0 0 > tests/pbf/countries-0-0-0.pbf.out.json.check
	cmp tests/pbf/countries-0-0-0.pbf.out.json.check tests/pbf/countries-0-0-0.pbf.out.json
	rm tests/pbf/countries-0-0-0.pbf.out tests/pbf/countries-0-0-0.pbf.out.json.check
	# Deduplication by feature ID
	./tippecanoe -z0 -f -e tests/pbf/1.json.dir -l layer tests/pbf/1.json
	./tippecanoe -z0 -f -e tests/pbf/2.json.dir -l layer tests/pbf/2.json
	./tippecanoe-overzoom -b0 -o tests/pbf/merged-nodedup.pbf -t 1/1/0 tests/pbf/1.json.dir/0/0/0.pbf 0/0/0 tests/pbf/2.json.dir/0/0/0.pbf 0/0/0
	./tippecanoe-decode tests/pbf/merged-nodedup.pbf 1 1 0 > tests/pbf/merged-nodedup.pbf.json.check
	cmp tests/pbf/merged-nodedup.pbf.json.check tests/pbf/merged-nodedup.pbf.json
	./tippecanoe-overzoom -b0 --deduplicate-by-id -o tests/pbf/merged-dedup.pbf -t 1/1/0 tests/pbf/1.json.dir/0/0/0.pbf 0/0/0 tests/pbf/2.json.dir/0/0/0.pbf 0/0/0
	./tippecanoe-decode tests/pbf/merged-dedup.pbf 1 1 0 > tests/pbf/merged-dedup.pbf.json.check
	cmp tests/pbf/merged-dedup.pbf.json.check tests/pbf/merged-dedup.pbf.json
	rm -r tests/pbf/1.json.dir tests/pbf/2.json.dir tests/pbf/merged-nodedup.pbf tests/pbf/merged-nodedup.pbf.json.check tests/pbf/merged-dedup.pbf tests/pbf/merged-dedup.pbf.json.check

json-tool-test: tippecanoe-json-tool
	./tippecanoe-json-tool -e GEOID10 tests/join-population/tabblock_06001420.json | sort > tests/join-population/tabblock_06001420.json.sort
	./tippecanoe-json-tool -c tests/join-population/population.csv tests/join-population/tabblock_06001420.json.sort > tests/join-population/tabblock_06001420.json.sort.joined
	./tippecanoe-json-tool --empty-csv-columns-are-null -c tests/join-population/population.csv tests/join-population/tabblock_06001420.json.sort > tests/join-population/tabblock_06001420-null.json.sort.joined
	cmp tests/join-population/tabblock_06001420.json.sort.joined tests/join-population/tabblock_06001420.json.sort.joined.standard
	cmp tests/join-population/tabblock_06001420-null.json.sort.joined tests/join-population/tabblock_06001420-null.json.sort.joined.standard
	rm -f tests/join-population/tabblock_06001420.json.sort tests/join-population/tabblock_06001420.json.sort.joined
	rm -f tests/join-population/tabblock_06001420-null.json.sort.joined
