# Leafmask — developer & packaging tasks.
# `make help` lists everything.

BIN        := leafmask
FEATURES   ?= full
PREFIX     ?= /usr/local
IMAGE      ?= leafmask
CARGO      := cargo
# proptest case count for `make test-property`; the default suite uses
# proptest's own default (256), CI runs 2048.
PROPTEST_CASES ?= 2048
# Blocking coverage floors. Keep in sync with the `coverage` job in
# .github/workflows/ci.yml — raise them when the real numbers rise, never
# lower them to make a red build green.
MIN_LINE_COVERAGE     ?= 85
MIN_FUNCTION_COVERAGE ?= 75

.DEFAULT_GOAL := help

.PHONY: help
help: ## Show this help
	@grep -hE '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | \
		awk 'BEGIN{FS=":.*?## "}{printf "  \033[1;36m%-16s\033[0m %s\n", $$1, $$2}'

.PHONY: build
build: ## Build the release binary with all features ($(FEATURES))
	$(CARGO) build --release --features "$(FEATURES)"

.PHONY: build-core
build-core: ## Build the lean default binary (no external backends)
	$(CARGO) build --release

.PHONY: test
test: ## Run the unit + property test suites (default features, no services)
	$(CARGO) test

.PHONY: test-mongo
test-mongo: ## Run unit + live integration tests (needs a MongoDB on :27017)
	$(CARGO) test --features mongo

.PHONY: test-mongo-matrix
test-mongo-matrix: ## Run the integration suite against every supported MongoDB version (needs Docker)
	./scripts/test-mongo-matrix.sh

.PHONY: test-storage
test-storage: ## Run S3/Azure backend tests against real containers (needs Docker)
	$(CARGO) test --features "s3,azure,integration-tests" \
		--test s3_integration --test azure_integration

.PHONY: test-property
test-property: ## Run the property suites with a high case count (slower, explores more)
	PROPTEST_CASES=$(PROPTEST_CASES) $(CARGO) test \
		--test property_dump_format --test property_transformers

.PHONY: test-all
test-all: test-mongo test-storage test-property ## Everything that can run locally (needs Docker + MongoDB)

.PHONY: coverage
coverage: ## Report coverage and enforce the CI thresholds (needs cargo-llvm-cov + MongoDB on :27017)
	$(CARGO) llvm-cov --features mongo --summary-only \
		--fail-under-lines $(MIN_LINE_COVERAGE) \
		--fail-under-functions $(MIN_FUNCTION_COVERAGE)

.PHONY: fmt
fmt: ## Format the code
	$(CARGO) fmt

.PHONY: lint
lint: ## Check formatting and run clippy on all shipped features
	$(CARGO) fmt --check
	$(CARGO) clippy --all-targets --features "$(FEATURES)" -- -D warnings

.PHONY: install
install: build ## Build and install to $(PREFIX)/bin
	install -d "$(PREFIX)/bin"
	install -m 0755 target/release/$(BIN) "$(PREFIX)/bin/$(BIN)"

.PHONY: uninstall
uninstall: ## Remove the installed binary
	rm -f "$(PREFIX)/bin/$(BIN)"

.PHONY: docker
docker: ## Build the Docker image ($(IMAGE))
	docker build -t "$(IMAGE)" .

.PHONY: clean
clean: ## Remove build artifacts
	$(CARGO) clean
	rm -rf dist
