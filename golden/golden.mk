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
