#!/usr/bin/env bash
set -euo pipefail

FIXTURE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MATERIALS="$FIXTURE/materials"
SERVER_HOSTNAME="${TABLEPRO_DRIVER_TLS_HOSTNAME:-localhost}"

if [[ "${1:-}" == "--force" ]]; then
  rm -rf "$MATERIALS"
fi

if [[ -f "$MATERIALS/server.pem" ]]; then
  echo "driver-tls materials already present in $MATERIALS"
  exit 0
fi

if ! command -v openssl >/dev/null 2>&1; then
  echo "missing required command: openssl" >&2
  exit 1
fi

mkdir -p "$MATERIALS"
cd "$MATERIALS"

openssl req -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes \
  -keyout ca.key -out ca.crt \
  -subj "/CN=TablePro driver TLS fixture CA" \
  -addext "basicConstraints=critical,CA:TRUE" >/dev/null 2>&1

openssl req -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes \
  -keyout other-ca.key -out other-ca.crt \
  -subj "/CN=TablePro unrelated CA" \
  -addext "basicConstraints=critical,CA:TRUE" >/dev/null 2>&1

cat > server.ext <<EXT
basicConstraints=CA:FALSE
extendedKeyUsage=serverAuth
subjectAltName=DNS:${SERVER_HOSTNAME},DNS:mongo.tablepro.test,DNS:redis.tablepro.test,DNS:mysql.tablepro.test,IP:127.0.0.1
EXT

openssl req -newkey rsa:2048 -nodes -keyout server.key -out server.csr \
  -subj "/CN=${SERVER_HOSTNAME}" >/dev/null 2>&1
openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial \
  -out server.crt -days 3650 -sha256 -extfile server.ext >/dev/null 2>&1
rm -f server.csr server.ext ca.srl

cat server.key server.crt > server.pem

chmod 600 server.key ca.key other-ca.key
chmod 644 server.crt ca.crt other-ca.crt server.pem

echo "wrote driver-tls materials to $MATERIALS"
