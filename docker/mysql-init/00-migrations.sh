#!/bin/bash
set -Eeuo pipefail

for migration in /docker-entrypoint-initdb.d/migrations/*.up.sql; do
  echo "Applying migration: ${migration##*/}"
  mysql --protocol=socket -uroot -p"${MYSQL_ROOT_PASSWORD}" "${MYSQL_DATABASE}" < "$migration"
done
