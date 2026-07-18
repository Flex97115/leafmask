# Leafmask — developer & packaging tasks.
# `make help` lists everything.

BIN        := leafmask
FEATURES   ?= full
PREFIX     ?= /usr/local
IMAGE      ?= leafmask
CARGO      := cargo

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
test: ## Run the unit test suite (default features)
	$(CARGO) test

.PHONY: test-mongo
test-mongo: ## Run unit + live integration tests (needs a MongoDB on :27017)
	$(CARGO) test --features mongo

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
