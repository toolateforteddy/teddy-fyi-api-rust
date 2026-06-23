.PHONY: build run dev test clean install init docker-build docker-run docker-run-i docker-clean docker-tag docker-push

DATABASE_URL ?= postgresql://postgres:postgres@localhost:5432/neondb

# Local Rust commands
init:
	@echo "Installing Rust toolchain..."
	curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

install:
	@echo "Fetching Rust dependencies..."
	@cargo fetch

build:
	cargo build

run:
	cargo run

dev:
	./scripts/dev.sh

test:
	DATABASE_URL="$(DATABASE_URL)" cargo test

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
