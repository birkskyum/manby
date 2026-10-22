# tippecanoe's .json.check recipes, run with manby. This runs in the scratch
# directory that the Makefile's test-golden sets up, where ./tippecanoe is
# manby and tests/ mirrors tippecanoe-rs's tests with the outputs written locally.

# Make patterns treat % specially, so the skip list is applied in the shell.
TESTS := $(shell for f in tests/*/out/*.json; do grep -qxF "$$f" skip.txt || echo "$$f"; done)
SPACE = $(NULL) $(NULL)

suffixes = json json.gz

all: $(addsuffix .check,$(TESTS))

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
