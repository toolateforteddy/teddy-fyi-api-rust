.PHONY: build run test clean install init docker-build docker-run docker-clean

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

test:
	cargo test

clean:
	cargo clean

# Docker commands
docker-build:
	docker build -t teddy-fyi-api-rust .

docker-run: docker-clean
	docker run -d \
		--init \
		-p 8080:8080 -e PORT=8080 \
		--name teddy-rust-server \
		teddy-fyi-api-rust

docker-run-i: docker-clean
	docker run -it \
		--init \
		-p 8080:8080 -e PORT=8080 \
		--name teddy-rust-server \
		teddy-fyi-api-rust

docker-clean:
	docker rm -f teddy-rust-server || true
