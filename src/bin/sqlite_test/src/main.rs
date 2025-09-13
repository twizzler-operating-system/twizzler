use rusqlite::{Connection, Result};
use std::time::Instant;

#[unsafe(no_mangle)]
unsafe extern "C" fn __dlapi_close() -> i32 { return 0; }
#[unsafe(no_mangle)]
unsafe extern "C" fn __dlapi_error() -> i32 { return 0; }
#[unsafe(no_mangle)]
unsafe extern "C" fn __dlapi_open() -> i32 { return 0; }
#[unsafe(no_mangle)]
unsafe extern "C" fn __dlapi_resolve() -> i32 { return 0; }
#[unsafe(no_mangle)]
unsafe extern "C" fn __dlapi_reverse() -> i32 { return 0; }

const NUM_INSERTS: usize = 1000;

fn benchmark_transient_sqlite() -> Result<(Connection, std::time::Duration)> {
    println!("Benchmarking transient SQLite in-memory database...");

    let conn = Connection::open_in_memory()?;

    // Create a transient table
    conn.execute(
        "CREATE TABLE test_table (
            id INTEGER,
            name TEXT,
            value INTEGER,
            data BLOB
        )",
        [],
    )?;
    
    let start = Instant::now();
    
    conn.execute("BEGIN TRANSACTION", [])?;
    for i in 0..NUM_INSERTS {
        conn.execute(
            "INSERT INTO test_table (id, name, value, data) VALUES (?1, ?2, ?3, ?4)",
            [
                &i,
                &format!("name_{}", i) as &dyn rusqlite::ToSql,
                &(i * 2),
                &vec![0u8; 100] as &dyn rusqlite::ToSql,
            ],
        )?;
    }
    conn.execute("COMMIT", [])?;
    
    let duration = start.elapsed();
    println!("Standard SQLite: Inserted {} records in {:?}", NUM_INSERTS, duration);

    Ok((conn, duration))
}

fn query_transient_sqlite(conn: &Connection) -> Result<std::time::Duration> {
    println!("Querying transient SQLite in-memory database...");
    
    let start = Instant::now();
    
    // Test different query types
    
    // 1. Count all records
    let mut stmt = conn.prepare("SELECT COUNT(*) FROM test_table")?;
    let count: i64 = stmt.query_row([], |row| row.get(0))?;
    println!("  Total records: {}", count);
    
    // 2. Select specific records by ID
    let mut stmt = conn.prepare("SELECT id, name, value FROM test_table WHERE id BETWEEN ? AND ?")?;
    let mut rows = stmt.query_map([&100, &110], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    
    let mut selected_count = 0;
    for row in rows {
        let _record = row?;
        selected_count += 1;
    }
    println!("  Selected records (ID 100-110): {}", selected_count);
    
    // 3. Aggregate query
    let mut stmt = conn.prepare("SELECT AVG(value), MAX(value), MIN(value) FROM test_table")?;
    let (avg, max, min): (f64, i64, i64) = stmt.query_row([], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })?;
    println!("  Value stats - Avg: {:.2}, Max: {}, Min: {}", avg, max, min);
    
    let duration = start.elapsed();
    println!("Standard SQLite queries completed in {:?}", duration);
    
    Ok(duration)
}

fn benchmark_file_sqlite() -> Result<(Connection, std::time::Duration)> {
    println!("Benchmarking SQLite file database...");
    
    // Remove existing test file if it exists
    let _ = std::fs::remove_file("benchmark_test.db");
    
    let conn = Connection::open("benchmark_test.db")?;
    
    // Create a standard table
    conn.execute(
        "CREATE TABLE test_table (
            id INTEGER,
            name TEXT,
            value INTEGER,
            data BLOB
        )",
        [],
    )?;
    
    let start = Instant::now();
    
    conn.execute("BEGIN TRANSACTION", [])?;
    for i in 0..NUM_INSERTS {
        conn.execute(
            "INSERT INTO test_table (id, name, value, data) VALUES (?1, ?2, ?3, ?4)",
            [
                &i,
                &format!("name_{}", i) as &dyn rusqlite::ToSql,
                &(i * 2),
                &vec![0u8; 100] as &dyn rusqlite::ToSql,
            ],
        )?;
    }
    conn.execute("COMMIT", [])?;
    
    let duration = start.elapsed();
    println!("File SQLite: Inserted {} records in {:?}", NUM_INSERTS, duration);

    Ok((conn, duration))
}

fn query_file_sqlite(conn: &Connection) -> Result<std::time::Duration> {
    println!("Querying SQLite file database...");
    
    let start = Instant::now();
    
    // Test different query types
    
    // 1. Count all records
    let mut stmt = conn.prepare("SELECT COUNT(*) FROM test_table")?;
    let count: i64 = stmt.query_row([], |row| row.get(0))?;
    println!("  Total records: {}", count);
    
    // 2. Select specific records by ID
    let mut stmt = conn.prepare("SELECT id, name, value FROM test_table WHERE id BETWEEN ? AND ?")?;
    let mut rows = stmt.query_map([&100, &110], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    
    let mut selected_count = 0;
    for row in rows {
        let _record = row?;
        selected_count += 1;
    }
    println!("  Selected records (ID 100-110): {}", selected_count);
    
    // 3. Aggregate query
    let mut stmt = conn.prepare("SELECT AVG(value), MAX(value), MIN(value) FROM test_table")?;
    let (avg, max, min): (f64, i64, i64) = stmt.query_row([], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })?;
    println!("  Value stats - Avg: {:.2}, Max: {}, Min: {}", avg, max, min);
    
    let duration = start.elapsed();
    println!("File SQLite queries completed in {:?}", duration);
    
    Ok(duration)
}

fn benchmark_transient_vtab() -> Result<(Connection, std::time::Duration)> {
    println!("Benchmarking Twizzler transient virtual table...");
    
    let conn = Connection::open_in_memory()?;
    conn.setup_twz_vtab();
    
    // Create a transient virtual table
    conn.execute(
        "CREATE VIRTUAL TABLE test_table USING twz_transient_vtab(
            id INTEGER,
            name TEXT,
            value INTEGER,
            data BLOB
        )",
        [],
    )?;
    
    let start = Instant::now();

    for i in 0..NUM_INSERTS {
        conn.execute(
            "INSERT INTO test_table (id, name, value, data) VALUES (?1, ?2, ?3, ?4)",
            [
                &i,
                &format!("name_{}", i) as &dyn rusqlite::ToSql,
                &(i * 2),
                &vec![0u8; 100] as &dyn rusqlite::ToSql,
            ],
        )?;
    }
    
    let duration = start.elapsed();
    println!("Transient VTab: Inserted {} records in {:?}", NUM_INSERTS, duration);

    Ok((conn, duration))
}

fn query_transient_vtab(conn: &Connection) -> Result<std::time::Duration> {
    println!("Querying Twizzler transient virtual table...");
    
    let start = Instant::now();
    
    // Test different query types
    
    // 1. Count all records
    let mut stmt = conn.prepare("SELECT COUNT(*) FROM test_table")?;
    let count: i64 = stmt.query_row([], |row| row.get(0))?;
    println!("  Total records: {}", count);
    
    // 2. Select specific records by ID
    let mut stmt = conn.prepare("SELECT id, name, value FROM test_table WHERE id BETWEEN ? AND ?")?;
    let mut rows = stmt.query_map([&100, &110], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    
    let mut selected_count = 0;
    for row in rows {
        let _record = row?;
        selected_count += 1;
    }
    println!("  Selected records (ID 100-110): {}", selected_count);
    
    // 3. Aggregate query
    let mut stmt = conn.prepare("SELECT AVG(value), MAX(value), MIN(value) FROM test_table")?;
    let (avg, max, min): (f64, i64, i64) = stmt.query_row([], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })?;
    println!("  Value stats - Avg: {:.2}, Max: {}, Min: {}", avg, max, min);
    
    let duration = start.elapsed();
    println!("Transient VTab queries completed in {:?}", duration);
    
    Ok(duration)
}

fn benchmark_persistent_vtab() -> Result<(Connection, std::time::Duration)> {
    println!("Benchmarking Twizzler persistent virtual table...");
    
    let conn = Connection::open_in_memory()?;
    conn.setup_twz_vtab();
    
    // Create a persistent virtual table
    conn.execute(
        "CREATE VIRTUAL TABLE test_table USING twz_persistent_vtab(
            id INTEGER,
            name TEXT,
            value INTEGER,
            data BLOB
        )",
        [],
    )?;
    
    let start = Instant::now();

    for i in 0..NUM_INSERTS {
        conn.execute(
            "INSERT INTO test_table (id, name, value, data) VALUES (?1, ?2, ?3, ?4)",
            [
                &i,
                &format!("name_{}", i) as &dyn rusqlite::ToSql,
                &(i * 2),
                &vec![0u8; 100] as &dyn rusqlite::ToSql,
            ],
        )?;
    }
    
    let duration = start.elapsed();
    println!("Persistent VTab: Inserted {} records in {:?}", NUM_INSERTS, duration);

    Ok((conn, duration))
}

fn query_persistent_vtab(conn: &Connection) -> Result<std::time::Duration> {
    println!("Querying Twizzler persistent virtual table...");
    
    let start = Instant::now();
    
    // Test different query types
    
    // 1. Count all records
    let mut stmt = conn.prepare("SELECT COUNT(*) FROM test_table")?;
    let count: i64 = stmt.query_row([], |row| row.get(0))?;
    println!("  Total records: {}", count);
    
    // 2. Select specific records by ID
    let mut stmt = conn.prepare("SELECT id, name, value FROM test_table WHERE id BETWEEN ? AND ?")?;
    let mut rows = stmt.query_map([&100, &110], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    
    let mut selected_count = 0;
    for row in rows {
        let _record = row?;
        selected_count += 1;
    }
    println!("  Selected records (ID 100-110): {}", selected_count);
    
    // 3. Aggregate query
    let mut stmt = conn.prepare("SELECT AVG(value), MAX(value), MIN(value) FROM test_table")?;
    let (avg, max, min): (f64, i64, i64) = stmt.query_row([], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })?;
    println!("  Value stats - Avg: {:.2}, Max: {}, Min: {}", avg, max, min);
    
    let duration = start.elapsed();
    println!("Persistent VTab queries completed in {:?}", duration);
    
    Ok(duration)
}

fn benchmark_file_persistence() -> Result<(std::time::Duration)> {
    println!("Benchmarking SQLite persistent storage for opening and querying existing data...");
    let start = Instant::now();
    let conn = Connection::open("benchmark_test.db")?;

    let query_duration = query_file_sqlite(&conn)?;
    let duration = start.elapsed();
    println!("Reopened and queried existing SQLite file database in {:?}", duration);
    Ok((duration))
}

fn benchmark_persistent_vtab_persistence() -> Result<(std::time::Duration)> {
    println!("Benchmarking Twizzler persistent virtual table for opening and querying existing data...");
    let start = Instant::now();
    let conn = Connection::open_in_memory()?;
    conn.setup_twz_vtab();
    
    // Recreate the persistent virtual table
    conn.execute(
        "CREATE VIRTUAL TABLE test_table USING twz_persistent_vtab(
            id INTEGER,
            name TEXT,
            value INTEGER,
            data BLOB
        )",
        [],
    )?;

    let query_duration = query_persistent_vtab(&conn)?;
    let duration = start.elapsed();
    println!("Reopened and queried existing Twizzler persistent virtual table in {:?}", duration);
    Ok((duration))
}

fn run_bench() -> Result<()> {
    println!("\n=== Batch Insert Performance Comparison ===");
    
    // Transient SQLite
    let (sqlite_conn, sqlite_duration) = benchmark_transient_sqlite()?;

    // File SQLite (Currently dysfunctional.)
    // let (file_conn, file_duration) = benchmark_file_sqlite()?;

    // Transient VTab
    let (transient_conn, transient_duration) = benchmark_transient_vtab()?;

    // Persistent VTab
    let (persistent_conn, persistent_duration) = benchmark_persistent_vtab()?;

    // Print comparison
    println!("\n=== Performance Summary ===");
    println!("Number of inserts: {}", NUM_INSERTS);
    println!("Standard SQLite:   {:?}", sqlite_duration);
    // println!("File SQLite:       {:?}", file_duration);
    println!("Transient VTab:    {:?}", transient_duration);
    println!("Persistent VTab:   {:?}", persistent_duration);
    
    println!("\nThroughput (records/second):");
    println!("Standard SQLite:   {:.0}", NUM_INSERTS as f64 / sqlite_duration.as_secs_f64());
    // println!("File SQLite:       {:.0}", NUM_INSERTS as f64 / file_duration.as_secs_f64());
    println!("Transient VTab:    {:.0}", NUM_INSERTS as f64 / transient_duration.as_secs_f64());
    println!("Persistent VTab:   {:.0}", NUM_INSERTS as f64 / persistent_duration.as_secs_f64());

    println!("\n=== Query Performance Comparison ===");

    let sqlite_query_duration = query_transient_sqlite(&sqlite_conn)?;
    // let file_query_duration = query_file_sqlite(&file_conn)?;
    let transient_query_duration = query_transient_vtab(&transient_conn)?;
    let persistent_query_duration = query_persistent_vtab(&persistent_conn)?;

    println!("\n=== Query Performance Summary ===");
    println!("Standard SQLite:   {:?}", sqlite_query_duration);
    // println!("File SQLite:       {:?}", file_query_duration);
    println!("Transient VTab:    {:?}", transient_query_duration);
    println!("Persistent VTab:   {:?}", persistent_query_duration);

    // // Benchmark reopening and querying existing data
    // drop(file_conn);
    // drop(persistent_conn);

    // // let file_persistence_duration = benchmark_file_persistence()?;
    // let persistent_vtab_persistence_duration = benchmark_persistent_vtab_persistence()?;

    // println!("\n=== Persistence Performance Summary ===");
    // // println!("Reopen & Query File SQLite:       {:?}", file_persistence_duration);
    // println!("Reopen & Query Persistent VTab:   {:?}", persistent_vtab_persistence_duration);
    
    Ok(())
}

fn cleanup() -> Result<()> {
    // Clean up test file
    // let _ = std::fs::remove_file("benchmark_test.db");

    // let mut nh = naming::dynamic_naming_factory().unwrap();
    // let _ = nh.remove("/data/vtab-test_table");

    Ok(())
}

fn main() -> Result<()> {
    println!("SQLite Performance Benchmark");
    println!("Testing with {} records\n", NUM_INSERTS);

    run_bench()?;

    cleanup()?;
    
    Ok(())
}