use crate::common::{do_flush, limbo_exec_rows, TempDatabase};
use std::path::Path;
use std::sync::Arc;
use turso_core::Connection;

#[test]
fn upsert_keeps_rowid_alias_out_of_record_body() -> anyhow::Result<()> {
    let db = TempDatabase::new_empty();
    let conn = db.connect_limbo();

    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, v INTEGER)")?;
    conn.execute("INSERT INTO t VALUES (1000000, 100)")?;
    conn.execute(
        "INSERT INTO t(id, v) VALUES (1000000, 200) ON CONFLICT(id) DO UPDATE SET v = 200",
    )?;
    assert_eq!(
        limbo_exec_rows(&conn, "SELECT id, v FROM t"),
        vec![vec![
            rusqlite::types::Value::Integer(1_000_000),
            rusqlite::types::Value::Integer(200),
        ]]
    );

    checkpoint(&db, &conn)?;
    assert_eq!(first_table_column_serial_type(&db.path, 2)?, 0);
    Ok(())
}

#[test]
fn drop_column_rewrite_keeps_rowid_alias_out_of_record_body() -> anyhow::Result<()> {
    let db = TempDatabase::new_empty();
    let conn = db.connect_limbo();

    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, v INTEGER, w INTEGER)")?;
    conn.execute("INSERT INTO t VALUES (1000000, 100, 7)")?;
    conn.execute("ALTER TABLE t DROP COLUMN w")?;
    assert_eq!(
        limbo_exec_rows(&conn, "SELECT id, v FROM t"),
        vec![vec![
            rusqlite::types::Value::Integer(1_000_000),
            rusqlite::types::Value::Integer(100),
        ]]
    );

    checkpoint(&db, &conn)?;
    assert_eq!(first_table_column_serial_type(&db.path, 2)?, 0);
    Ok(())
}

#[test]
fn alter_column_rewrite_keeps_rowid_alias_out_of_record_body() -> anyhow::Result<()> {
    let db = TempDatabase::new_empty();
    let conn = db.connect_limbo();

    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, v INTEGER)")?;
    conn.execute("INSERT INTO t VALUES (1000000, 100)")?;
    conn.execute("ALTER TABLE t ALTER COLUMN v TO value INTEGER")?;
    assert_eq!(
        limbo_exec_rows(&conn, "SELECT id, value FROM t"),
        vec![vec![
            rusqlite::types::Value::Integer(1_000_000),
            rusqlite::types::Value::Integer(100),
        ]]
    );

    checkpoint(&db, &conn)?;
    assert_eq!(first_table_column_serial_type(&db.path, 2)?, 0);
    Ok(())
}

fn checkpoint(db: &TempDatabase, conn: &Arc<Connection>) -> anyhow::Result<()> {
    do_flush(conn, db)?;
    conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")?;
    do_flush(conn, db)
}

fn first_table_column_serial_type(path: &Path, page_number: usize) -> anyhow::Result<u64> {
    let file = std::fs::read(path)?;
    let encoded_page_size = u16::from_be_bytes([file[16], file[17]]);
    let page_size = if encoded_page_size == 1 {
        65_536
    } else {
        usize::from(encoded_page_size)
    };
    let page_start = (page_number - 1) * page_size;
    let page = &file[page_start..page_start + page_size];
    anyhow::ensure!(page[0] == 0x0d, "expected a table leaf page");

    let cell_count = usize::from(u16::from_be_bytes([page[3], page[4]]));
    anyhow::ensure!(
        cell_count == 1,
        "expected one table row, found {cell_count}"
    );
    let cell_offset = usize::from(u16::from_be_bytes([page[8], page[9]]));

    let (payload_size, payload_size_bytes) = read_varint(page, cell_offset)?;
    anyhow::ensure!(payload_size > 0, "table cell has an empty payload");
    let rowid_offset = cell_offset + payload_size_bytes;
    let cursor = rowid_offset + read_varint(page, rowid_offset)?.1;
    let (header_size, header_size_bytes) = read_varint(page, cursor)?;
    anyhow::ensure!(header_size >= header_size_bytes as u64 + 1);
    let serial_type_offset = cursor + header_size_bytes;
    Ok(read_varint(page, serial_type_offset)?.0)
}

fn read_varint(bytes: &[u8], start: usize) -> anyhow::Result<(u64, usize)> {
    let mut value = 0u64;
    for index in 0..8 {
        let byte = *bytes
            .get(start + index)
            .ok_or_else(|| anyhow::anyhow!("truncated varint"))?;
        value = (value << 7) | u64::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            return Ok((value, index + 1));
        }
    }

    let byte = *bytes
        .get(start + 8)
        .ok_or_else(|| anyhow::anyhow!("truncated 9-byte varint"))?;
    value = (value << 8) | u64::from(byte);
    Ok((value, 9))
}
