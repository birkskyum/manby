# manby's tests.
#
#   make test          the unit tests and the golden tests
#   make test-golden   tippecanoe's golden .json.check tests and the other
#                      recipes that need only tippecanoe, run with manby
#   make test-geobuf   the golden tests again from Geobuf input
#
# The golden tests come from tippecanoe-rs (TIPPECANOE_RS), whose
# tippecanoe-decode decodes the output. Tests that manby does not pass yet
# are listed in golden/skip.txt, with the reason.

CARGO ?= cargo
TIPPECANOE_RS ?= ../tippecanoe-rs
RELEASE = $(TIPPECANOE_RS)/target/release
DECODE = $(RELEASE)/tippecanoe-decode
GOLDEN = target/golden

.PHONY: all test unit test-golden test-geobuf golden-setup manby tools

all: manby

manby:
	$(CARGO) build --release

# tippecanoe-rs's tools that the recipes use
tools:
	cd $(TIPPECANOE_RS) && $(CARGO) build --release --bin tippecanoe-decode --bin tippecanoe-json-tool --example json2geobuf

unit:
	$(CARGO) test --release

test: unit test-golden test-geobuf

golden-setup: manby tools
	golden/setup.sh $(TIPPECANOE_RS)/tests $(GOLDEN) target/release/manby $(RELEASE)
	cp golden/skip.txt $(GOLDEN)/skip.txt

test-golden: golden-setup
	$(MAKE) -C $(GOLDEN) -f $(CURDIR)/golden/golden.mk -k all

test-geobuf: golden-setup
	$(MAKE) -C $(GOLDEN) -f $(CURDIR)/golden/golden.mk -k -j1 geobuf-test
