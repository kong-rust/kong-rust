#!/usr/bin/env bash

# PostgreSQL 初始化脚本
# 由 docker-entrypoint-initdb.d 自动执行，创建开发和测试数据库

for database in $(echo $POSTGRES_DBS | tr ',' ' '); do
  echo "创建数据库: $database"
  psql -U $POSTGRES_USER <<-EOSQL
    SELECT 'CREATE DATABASE $database' WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = '$database')\gexec
    GRANT ALL PRIVILEGES ON DATABASE $database TO $POSTGRES_USER;
EOSQL
done
