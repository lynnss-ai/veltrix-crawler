#!/usr/bin/env python3
"""veltrix-crawler:SQLite -> PostgreSQL 一次性数据迁移。

用法:
    python migrate-sqlite-to-pg.py [--sqlite PATH] [--pg DSN] [--dry-run]

- --sqlite  源库文件路径,默认桌面端数据目录下的 veltrix.db
- --pg      目标 PG 连接串;缺省读环境变量 VELTRIX_DATABASE_URL
- 目标库的表结构必须先由应用创建(用 VELTRIX_DATABASE_URL 启动一次
  veltrix-server 或桌面端,init_schema 会自动建表),本脚本只搬数据,
  每张表 INSERT ... ON CONFLICT DO NOTHING,可重复执行(幂等)。

设计说明:
- 无物理外键(项目约定),表间无依赖顺序,逐表独立事务提交;
- 主键多为字符串,个别表是自增整数,迁移后统一重置序列(setval);
- SQLite 只读打开(mode=ro),应用运行中也可安全执行(WAL 模式)。
"""

import argparse
import os
import sqlite3
import sys

import psycopg2
from psycopg2.extras import execute_values

# 桌面端默认数据目录(Tauri identifier 下的配置目录)
DEFAULT_SQLITE = os.path.expandvars(
    r"%APPDATA%\com.lynns.veltrix-crawler\veltrix-crawler\veltrix.db"
)
BATCH_SIZE = 500


def open_sqlite(path: str) -> sqlite3.Connection:
    if not os.path.isfile(path):
        sys.exit(f"源库不存在: {path}")
    conn = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    return conn


def list_tables(lite: sqlite3.Connection) -> list[str]:
    rows = lite.execute(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
    ).fetchall()
    return [r[0] for r in rows]


def pg_columns(pg, table: str) -> dict[str, str]:
    """返回 {列名: data_type}。"""
    cur = pg.cursor()
    cur.execute(
        "SELECT column_name, data_type FROM information_schema.columns "
        "WHERE table_schema='public' AND table_name=%s",
        (table,),
    )
    cols = {name: dtype for name, dtype in cur.fetchall()}
    cur.close()
    return cols


def sqlite_columns(lite: sqlite3.Connection, table: str) -> list[str]:
    return [r[1] for r in lite.execute(f'PRAGMA table_info("{table}")').fetchall()]


def copy_table(lite, pg, table: str, dry_run: bool) -> tuple[int, int]:
    """搬运单表,返回 (读取行数, 写入行数)。"""
    lite_cols = sqlite_columns(lite, table)
    pg_cols = pg_columns(pg, table)
    if not pg_cols:
        print(f"  [跳过] PG 中不存在表 {table}(请先用应用建表)")
        return (0, 0)

    cols = [c for c in lite_cols if c in pg_cols]
    dropped = [c for c in lite_cols if c not in pg_cols]
    if dropped:
        print(f"  [警告] {table} 有列在 PG 中不存在,未搬运: {dropped}")

    bool_cols = {c for c in cols if pg_cols[c] == "boolean"}
    quoted = ", ".join(f'"{c}"' for c in cols)
    insert_sql = f'INSERT INTO "{table}" ({quoted}) VALUES %s ON CONFLICT DO NOTHING'

    total, written = 0, 0
    cur = pg.cursor()
    lite_cur = lite.execute(f'SELECT {quoted} FROM "{table}"')
    while True:
        batch = lite_cur.fetchmany(BATCH_SIZE)
        if not batch:
            break
        total += len(batch)
        if dry_run:
            continue
        if bool_cols:
            idx = [cols.index(c) for c in bool_cols]
            batch = [
                tuple(bool(v[i]) if i in idx and v[i] is not None else v[i] for i in range(len(v)))
                for v in batch
            ]
        execute_values(cur, insert_sql, batch, page_size=BATCH_SIZE)
        written += cur.rowcount if cur.rowcount and cur.rowcount > 0 else 0
    cur.close()
    if not dry_run:
        pg.commit()
    return (total, written)


def reset_sequences(pg) -> None:
    """自增主键表:迁移带显式 id 后序列仍指向旧值,重置为 max(id)。"""
    cur = pg.cursor()
    cur.execute(
        "SELECT table_name, column_name FROM information_schema.columns "
        "WHERE table_schema='public' AND column_default LIKE 'nextval%'"
    )
    for table, col in cur.fetchall():
        cur.execute(
            f"SELECT setval(pg_get_serial_sequence('\"{table}\"','{col}'), "
            f'COALESCE((SELECT MAX("{col}") FROM "{table}"), 1))'
        )
    cur.close()
    pg.commit()


def main() -> None:
    ap = argparse.ArgumentParser(description="SQLite -> PostgreSQL 数据迁移")
    ap.add_argument("--sqlite", default=DEFAULT_SQLITE, help="源 SQLite 文件路径")
    ap.add_argument("--pg", default=os.environ.get("VELTRIX_DATABASE_URL", ""),
                    help="目标 PG 连接串(默认读 VELTRIX_DATABASE_URL)")
    ap.add_argument("--dry-run", action="store_true", help="只统计行数不写入")
    args = ap.parse_args()

    if not args.pg.startswith(("postgres://", "postgresql://")):
        sys.exit("请通过 --pg 或 VELTRIX_DATABASE_URL 提供 postgres:// 连接串")

    lite = open_sqlite(args.sqlite)
    pg = psycopg2.connect(args.pg)
    print(f"源: {args.sqlite}\n目标: {args.pg.split('@')[-1]}\n")

    grand_total = 0
    for table in list_tables(lite):
        total, written = copy_table(lite, pg, table, args.dry_run)
        grand_total += total
        suffix = "(dry-run)" if args.dry_run else f"写入 {written}"
        print(f"  {table}: 读取 {total} 行, {suffix}")

    if not args.dry_run:
        reset_sequences(pg)
        print("\n序列已重置。")
    print(f"完成,共 {grand_total} 行。")

    lite.close()
    pg.close()


if __name__ == "__main__":
    main()
