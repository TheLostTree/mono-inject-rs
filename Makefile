

mono_lib: 
	cd mono_load && cargo build --release

mono_inject:
	cargo build --release

clean:
	cargo clean && cd mono_load && cargo clean

everything: mono_lib mono_inject
	mkdir bin
	cp mono_load/target/release/mono_lib.dll bin/
	cp target/release/mono_inject.exe bin/
