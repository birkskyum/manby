# manby's tests.
#
#   make test          the unit tests and the golden tests
#   make test-golden   tippecanoe's golden .json.check tests, run with manby
#
# The golden tests come from tippecanoe-rs (TIPPECANOE_RS), whose
# tippecanoe-decode decodes the output. Tests that manby does not pass yet
# are listed in golden/skip.txt, with the reason.

CARGO ?= cargo
TIPPECANOE_RS ?= ../tippecanoe-rs
DECODE ?= $(TIPPECANOE_RS)/target/release/tippecanoe-decode
GOLDEN = target/golden

.PHONY: all test unit test-golden manby decode

all: manby

manby:
	$(CARGO) build --release

decode: $(DECODE)

$(DECODE):
	cd $(TIPPECANOE_RS) && $(CARGO) build --release --bin tippecanoe-decode

unit:
	$(CARGO) test --release

test: unit test-golden

test-golden: manby $(DECODE)
	golden/setup.sh $(TIPPECANOE_RS)/tests $(GOLDEN) target/release/manby $(DECODE)
	cp golden/skip.txt $(GOLDEN)/skip.txt
	$(MAKE) -C $(GOLDEN) -f $(CURDIR)/golden/golden.mk -k all
