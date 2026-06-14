#!/bin/bash
set -e

# Detect user name
USER_NAME="${USER:-$(whoami)}"
if [ -z "$USER_NAME" ]; then
  echo "Error: Could not detect username. Please set the USER environment variable."
  exit 1
fi

echo "User detected: ${USER_NAME}"

# 1. Fetch project ID
echo "Fetching Neon project ID..."
PROJECT_ID=$(npx neonctl branches list -o json | jq -r '.[0].project_id')
if [ -z "$PROJECT_ID" ] || [ "$PROJECT_ID" = "null" ]; then
  echo "Error: Could not retrieve Neon project ID. Make sure you are authenticated with 'npx neonctl auth'."
  exit 1
fi
echo "Project ID detected: ${PROJECT_ID}"

# 2. Find and delete orphaned branches matching dev-${USER_NAME}-*
echo "Scanning for orphaned dev branches for user ${USER_NAME}..."
ORPHANED_BRANCHES=$(npx neonctl branches list --project-id "$PROJECT_ID" -o json | jq -r --arg prefix "dev-${USER_NAME}-" '.[] | select(.name | startswith($prefix)) | .id')

if [ -n "$ORPHANED_BRANCHES" ]; then
  for BRANCH_ID in $ORPHANED_BRANCHES; do
    echo "Deleting orphaned branch: ${BRANCH_ID}"
    npx neonctl branches delete "$BRANCH_ID" --project-id "$PROJECT_ID"
  done
else
  echo "No orphaned dev branches found for user ${USER_NAME}."
fi

# 3. Create new branch cloned from production
TIMESTAMP=$(date +%s)
NEW_BRANCH_NAME="dev-${USER_NAME}-${TIMESTAMP}"

# Calculate 24 hours from now for expiration
if [[ "$OSTYPE" == "darwin"* ]]; then
  EXPIRY_DATE=$(date -v+24H -u +"%Y-%m-%dT%H:%M:%SZ")
else
  EXPIRY_DATE=$(date -d "+24 hours" -u +"%Y-%m-%dT%H:%M:%SZ")
fi

echo "Creating new dev branch: ${NEW_BRANCH_NAME} (expires at ${EXPIRY_DATE})..."
npx neonctl branches create --project-id "$PROJECT_ID" --name "$NEW_BRANCH_NAME" --parent "production" --expires-at "$EXPIRY_DATE"

# 4. Retrieve connection string for the new branch
echo "Fetching connection string..."
CONN_STR=$(npx neonctl connection-string "$NEW_BRANCH_NAME" --project-id "$PROJECT_ID")
if [ -z "$CONN_STR" ]; then
  echo "Error: Could not retrieve connection string for branch ${NEW_BRANCH_NAME}."
  exit 1
fi

# Append options required for PgBouncer/Neon compatibility (statement cache size, prepared statement cache size)
if [[ "$CONN_STR" == *"?"* ]]; then
  CONN_STR="${CONN_STR}&statement_cache_size=0&prepared_statement_cache_size=0"
else
  CONN_STR="${CONN_STR}?statement_cache_size=0&prepared_statement_cache_size=0"
fi

# 5. Update .env file
echo "Updating .env file..."
if [ ! -f .env ]; then
  if [ -f .env.example ]; then
    cp .env.example .env
  else
    touch .env
  fi
fi

# Helper function to append to .env safely with newline handling
append_to_env() {
  local line="$1"
  # Ensure there is a newline at the end of the file before appending
  [ -n "$(tail -c1 .env 2>/dev/null)" ] && echo "" >> .env
  echo "$line" >> .env
}

# Replace DATABASE_URL line in .env
if grep -q "^DATABASE_URL=" .env; then
  grep -v "^DATABASE_URL=" .env > .env.tmp || true
  mv .env.tmp .env
fi
append_to_env "DATABASE_URL=\"${CONN_STR}\""

# Set SQLX_OFFLINE to false in dev so it connects to the DB
if grep -q "^SQLX_OFFLINE=" .env; then
  grep -v "^SQLX_OFFLINE=" .env > .env.tmp || true
  mv .env.tmp .env
fi
append_to_env "SQLX_OFFLINE=false"

# Ensure JWT_SECRET and GEMINI_API_KEY exist in .env
if ! grep -q "^JWT_SECRET=" .env; then
  append_to_env "JWT_SECRET=\"dev_secret_key_123\""
  echo "Added fallback JWT_SECRET placeholder to .env"
fi

if ! grep -q "^GEMINI_API_KEY=" .env; then
  append_to_env "GEMINI_API_KEY=\"dev_placeholder_api_key\""
  echo "Added fallback GEMINI_API_KEY placeholder to .env"
fi

# Set REDIS_URL in dev so it connects to local Redis
if grep -q "^REDIS_URL=" .env; then
  grep -v "^REDIS_URL=" .env > .env.tmp || true
  mv .env.tmp .env
fi
append_to_env "REDIS_URL=\"redis://127.0.0.1:6379\""

# Start Redis / Valkey container if it is not running
echo "Checking local Redis dev container..."
if command -v docker &> /dev/null; then
  if ! docker ps --format '{{.Names}}' | grep -q "^teddy-redis-dev$"; then
    if docker ps -a --format '{{.Names}}' | grep -q "^teddy-redis-dev$"; then
      echo "Starting existing teddy-redis-dev container..."
      docker start teddy-redis-dev
    else
      echo "Running new teddy-redis-dev container..."
      docker run -d --name teddy-redis-dev -p 6379:6379 valkey/valkey:7.2 || docker run -d --name teddy-redis-dev -p 6379:6379 redis:alpine
    fi
  else
    echo "teddy-redis-dev container is already running."
  fi
else
  echo "Docker command not found, skipping starting local Redis container."
fi


# 6. Run migrations to ensure it has latest changes
echo "Running database migrations..."
if command -v sqlx &> /dev/null; then
  DATABASE_URL="${CONN_STR}" sqlx migrate run
else
  # Try Cargo's local path
  if [ -f "$HOME/.cargo/bin/sqlx" ]; then
    DATABASE_URL="${CONN_STR}" "$HOME/.cargo/bin/sqlx" migrate run
  else
    echo "sqlx-cli not found, skipping explicit migrations run. (Rust server will apply them on startup)"
  fi
fi

echo "Dev database setup successfully for branch: ${NEW_BRANCH_NAME}!"

# 7. Load environment variables from .env and start the server
echo "Starting development server..."
export $(grep -v '^#' .env | xargs)

if command -v cargo-watch &> /dev/null || cargo --list | grep -q "watch"; then
  echo "cargo-watch detected! Running server with hot-reload..."
  exec cargo watch -x run
else
  echo "cargo-watch not found. Starting development server without hot-reload..."
  exec cargo run
fi
