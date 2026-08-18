#!/usr/bin/env bash
set -euo pipefail

FIXTURE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MATERIALS="$FIXTURE/materials"
DB_HOSTNAME="${TABLEPRO_FIXTURE_DB_HOSTNAME:-db.tablepro.test}"

if [[ "${1:-}" == "--force" ]]; then
  rm -rf "$MATERIALS" "$FIXTURE/state"
fi

if [[ -f "$MATERIALS/server.crt" && -f "$MATERIALS/client_ed25519_key" ]]; then
  echo "fixture materials already present in $MATERIALS"
  exit 0
fi

for command_name in openssl ssh-keygen; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "missing required command: $command_name" >&2
    exit 1
  fi
done

mkdir -p "$MATERIALS"
cd "$MATERIALS"

openssl req -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes \
  -keyout ca.key -out ca.crt \
  -subj "/CN=TablePro release fixture CA" \
  -addext "basicConstraints=critical,CA:TRUE" >/dev/null 2>&1

openssl req -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes \
  -keyout other-ca.key -out other-ca.crt \
  -subj "/CN=TablePro unrelated CA" \
  -addext "basicConstraints=critical,CA:TRUE" >/dev/null 2>&1

cat > server.ext <<EXT
basicConstraints=CA:FALSE
extendedKeyUsage=serverAuth
subjectAltName=DNS:${DB_HOSTNAME},DNS:localhost
EXT

openssl req -newkey rsa:2048 -nodes -keyout server.key -out server.csr \
  -subj "/CN=${DB_HOSTNAME}" >/dev/null 2>&1
openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial \
  -out server.crt -days 3650 -sha256 -extfile server.ext >/dev/null 2>&1
rm -f server.csr server.ext ca.srl

chmod 600 server.key ca.key other-ca.key
chmod 644 server.crt ca.crt other-ca.crt

ssh-keygen -q -t ed25519 -N "" -C "tablepro-fixture-host" -f ssh_host_ed25519_key
ssh-keygen -q -t ed25519 -N "" -C "tablepro-fixture-client" -f client_ed25519_key

echo "wrote fixture materials to $MATERIALS"
echo "the SSH host key changed; the release script uses a fixture-local known_hosts file"
