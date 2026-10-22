# tippecanoe's .json.check tests, each run twice with manby, once writing MVT
# and once writing MLT, then compared tile by tile with examples/mlt_verify.
# This runs in the scratch directory that the Makefile's test-golden-mlt sets
# up, where ./tippecanoe is manby (MVT), ./manby-mlt is manby built with the
# mlt feature, and ./mlt_verify is the comparison.
#
# A test fails when the MLT tiles differ from the MVT tiles. Where a tile
# reaches a size limit, the limit is measured on the MLT bytes, so such a
# tile can keep other features; mlt-sizes.txt lists the tests where that is
# the only difference.

TESTS := $(shell for f in tests/*/out/*.json; do grep -qxF "$$f" skip.txt || echo "$$f"; done)
SPACE = $(NULL) $(NULL)
suffixes = json json.gz

all: $(addsuffix .mltcheck,$(TESTS))

testargs = \
    $(subst %20,' ',\
        $(subst %22,'"',\
            $(subst %3a,:,\
                $(subst %3d,=,\
                    $(subst %2f,/,\
                        $(subst %7b,'{',\
                            $(subst %5f,'_',\
                                $(subst _, ,$(1)))))))))

args = $(call testargs,$(patsubst %.json.mltcheck,%,$(word 4,$(subst /, ,$@))))
inputs = $(foreach suffix,$(suffixes),$(sort $(wildcard $(subst $(SPACE),/,$(wordlist 1,2,$(subst /, ,$@)))/*.$(suffix))))

%.json.mltcheck:
	./tippecanoe -q -a@ -f -o $@.mvt.pmtiles $(args) $(inputs) < /dev/null
	./manby-mlt -q -a@ -f -o $@.mlt.pmtiles --tile-format=mlt $(args) $(inputs) < /dev/null
	./mlt_verify $@.mvt.pmtiles $@.mlt.pmtiles > $@.out || { \
		./tippecanoe -q -a@ -f -pk -o $@.mvt.pmtiles $(args) $(inputs) < /dev/null && \
		./manby-mlt -q -a@ -f -pk -o $@.mlt.pmtiles --tile-format=mlt $(args) $(inputs) < /dev/null && \
		./mlt_verify $@.mvt.pmtiles $@.mlt.pmtiles >> $@.out && echo "$@" >> mlt-sizes.txt; } || { cat $@.out; false; }
	rm -f $@.out $@.mvt.pmtiles $@.mlt.pmtiles
