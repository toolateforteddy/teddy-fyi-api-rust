.PHONY: build run run-dev-auth dev test test-dev-auth clean install init docker-build docker-run docker-run-i docker-clean docker-tag docker-push

DATABASE_URL ?= postgresql://postgres:postgres@localhost:5432/neondb

# The `mock.` login bypass is compiled in only under this cargo feature, so it is absent
# from every build that does not name it -- `build`, `run`, `test` and the Dockerfile.
# Local development needs it, because a laptop has no Google client ID to validate against.
# See src/auth/dev_bypass.rs and the README.
DEV_AUTH_FEATURES ?= --features dev-auth

# Local Rust commands
init:
	@echo "Installing Rust toolchain..."
	curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

install:
	@echo "Fetching Rust dependencies..."
	@cargo fetch

build:
	cargo build

# Production-shaped: no dev bypass, so it needs real Google client IDs configured or it
# will refuse to start. Use `run-dev-auth` (or `dev`) for ordinary local work.
run:
	cargo run

run-dev-auth:
	cargo run $(DEV_AUTH_FEATURES)

dev:
	./scripts/dev.sh

# CI runs this one, so it is the production feature set: it is what proves the shipped
# binary rejects `mock.` tokens.
test:
	DATABASE_URL="$(DATABASE_URL)" cargo test

# The other half of the matrix. Both are worth running before pushing a change to
# src/auth/dev_bypass.rs, since each configuration compiles tests the other does not.
test-dev-auth:
	DATABASE_URL="$(DATABASE_URL)" cargo test $(DEV_AUTH_FEATURES)

clean:
	cargo clean

prepare:
	cargo sqlx prepare -- --tests

# Docker configuration parameters
REGISTRY ?= gcr.io
PROJECT_ID ?= melodic-sunbeam-164916
IMAGE_NAME ?= teddy-fyi-api-rust
VERSION ?= latest
BUILDER ?= docker
BUILD_ARGS ?=

# Docker commands
docker-build:
	$(BUILDER) build $(BUILD_ARGS) -t $(IMAGE_NAME):$(VERSION) .

docker-tag: docker-build
	docker tag $(IMAGE_NAME):$(VERSION) $(REGISTRY)/$(PROJECT_ID)/$(IMAGE_NAME):latest
	docker tag $(IMAGE_NAME):$(VERSION) $(REGISTRY)/$(PROJECT_ID)/$(IMAGE_NAME):$(VERSION)

docker-push: docker-tag
	docker push $(REGISTRY)/$(PROJECT_ID)/$(IMAGE_NAME):latest
	docker push $(REGISTRY)/$(PROJECT_ID)/$(IMAGE_NAME):$(VERSION)

docker-run: docker-clean
	docker run -d \
		--init \
		-p 8080:8080 -e PORT=8080 \
		--name teddy-rust-server \
		$(IMAGE_NAME):$(VERSION)

docker-run-i: docker-clean
	docker run -it \
		--init \
		-p 8080:8080 -e PORT=8080 \
		--name teddy-rust-server \
		$(IMAGE_NAME):$(VERSION)

docker-clean:
	docker rm -f teddy-rust-server || true
