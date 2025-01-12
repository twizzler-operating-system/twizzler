PREFIX := /usr/local

all:
	@cargo b --release

symlink:
	ln -sf $(CURDIR)/target/release/obliviate $(PREFIX)/bin/.
	ln -sf $(CURDIR)/target/release/obliviate-trap $(PREFIX)/bin/.
	ln -sf $(CURDIR)/target/release/liblethe.so $(PREFIX)/lib/.
	ln -sf $(CURDIR)/target/release/liblorax.so $(PREFIX)/lib/.
	ln -sf $(CURDIR)/target/release/libusdb.so $(PREFIX)/lib/.

install:
	@mkdir -p $(PREFIX)/bin
	@mkdir -p $(PREFIX)/lib
	cp $(CURDIR)/target/release/obliviate $(PREFIX)/bin/.
	cp $(CURDIR)/target/release/obliviate-trap $(PREFIX)/bin/.
	cp $(CURDIR)/target/release/liblethe.so $(PREFIX)/lib/.
	cp $(CURDIR)/target/release/liblorax.so $(PREFIX)/lib/.
	cp $(CURDIR)/target/release/libusdb.so $(PREFIX)/lib/.

uninstall:
	rm -f $(PREFIX)/bin/obliviate
	rm -f $(PREFIX)/bin/obliviate-trap
	rm -f $(PREFIX)/lib/liblethe.so
	rm -f $(PREFIX)/lib/liblorax.so
	rm -f $(PREFIX)/lib/libusdb.so

clean:
	@cargo clean

.PHONY: all symlink install uninstall clean
