all: test
.PHONY: all

build:
	@cargo build --all
.PHONY: build

doc:
	@cargo doc
.PHONY: doc

check-all-features:
	@echo 'ALL FEATURES'
	@RUSTFLAGS=-Dwarnings cargo check --all-features
.PHONY: check-all-features

check-no-default-features:
	@echo 'NO DEFAULT FEATURES'
	@RUSTFLAGS=-Dwarnings cargo check --no-default-features
.PHONY: check-no-default-features

check-default-features:
	@echo 'DEFAULT FEATURES'
	@RUSTFLAGS=-Dwarnings cargo check
.PHONY: check-default-features

check-failure:
	@echo 'NO CLIENT + FAILURE'
	@RUSTFLAGS=-Dwarnings cargo check --no-default-features --features 'with_failure'
.PHONY: check-failure

check-log:
	@echo 'NO CLIENT + LOG'
	@RUSTFLAGS=-Dwarnings cargo check --no-default-features --features 'with_log'
.PHONY: check-log

check-panic:
	@echo 'NO CLIENT + PANIC'
	@RUSTFLAGS=-Dwarnings cargo check --no-default-features --features 'with_panic'
.PHONY: check-panic

check-error-chain:
	@echo 'NO CLIENT + ERROR_CHAIN'
	@RUSTFLAGS=-Dwarnings cargo check --no-default-features --features 'with_error_chain'
.PHONY: check-error-chain

check-all-impls:
	@echo 'NO CLIENT + ALL IMPLS'
	@RUSTFLAGS=-Dwarnings cargo check --no-default-features --features 'with_failure,with_log,with_panic,with_error_chain'
.PHONY: check-all-impls

checkall: check-all-features check-no-default-features check-default-features check-failure check-log check-panic check-error-chain check-all-impls
.PHONY: checkall

cargotest:
	@echo 'TESTSUITE'
	@cargo test --all --all-features
.PHONY: cargotest

test: checkall cargotest
.PHONY: test

format-check:
	@cargo fmt -- --write-mode diff
.PHONY: format-check