use rusqlite::{Connection, Result, Transaction, params, named_params};
use std::collections::HashMap;

#[derive(Debug)]
struct Person {
    id: i32,
    name: String,
    age: Option<i32>,
    email: String,
    data: Option<Vec<u8>>,
    score: f64,
}

#[derive(Debug)]
struct Department {
    id: i32,
    name: String,
    budget: f64,
}

fn main() -> Result<()> {
    println!("=== SQLite Test Program ===");
    
    // Test 1: Basic operations
    test_basic_operations()?;
    
    // Test 2: Transactions
    test_transactions()?;
    
    // Test 3: Prepared statements and parameters
    test_prepared_statements()?;
    
    // Test 4: Joins and complex queries
    test_joins_and_aggregates()?;
    
    // Test 5: Blob operations
    test_blob_operations()?;
    
    // Test 6: User-defined functions
    test_user_functions()?;

    // Test 7: Persistent data
    // test_persistent_data()?;
    
    println!("=== All tests completed successfully! ===");
    Ok(())
}

fn test_basic_operations() -> Result<()> {
    println!("\n--- Testing Basic Operations ---");
    let conn = Connection::open_in_memory()?;

    // Create tables with various data types
    conn.execute(
        "CREATE TABLE person (
            id    INTEGER PRIMARY KEY AUTOINCREMENT,
            name  TEXT NOT NULL,
            age   INTEGER,
            email TEXT UNIQUE NOT NULL,
            data  BLOB,
            score REAL DEFAULT 0.0,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )",
        [],
    )?;

    // Insert multiple people
    let people = vec![
        ("Alice Johnson", Some(28), "alice@example.com", 95.5),
        ("Bob Smith", Some(34), "bob@example.com", 87.2),
        ("Charlie Brown", None, "charlie@example.com", 92.8),
        ("Diana Prince", Some(29), "diana@example.com", 98.1),
    ];

    for (name, age, email, score) in people {
        conn.execute(
            "INSERT INTO person (name, age, email, score) VALUES (?1, ?2, ?3, ?4)",
            params![name, age, email, score],
        )?;
    }

    // Query and display all people
    let mut stmt = conn.prepare("SELECT id, name, age, email, score FROM person ORDER BY score DESC")?;
    let person_iter = stmt.query_map([], |row| {
        Ok(Person {
            id: row.get(0)?,
            name: row.get(1)?,
            age: row.get(2)?,
            email: row.get(3)?,
            data: None,
            score: row.get(4)?,
        })
    })?;

    println!("All people (ordered by score):");
    for person in person_iter {
        println!("  {:?}", person.unwrap());
    }

    Ok(())
}

fn test_transactions() -> Result<()> {
    println!("\n--- Testing Transactions ---");
    let mut conn = Connection::open_in_memory()?;

    conn.execute(
        "CREATE TABLE accounts (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            balance REAL NOT NULL
        )",
        [],
    )?;

    // Insert initial accounts
    conn.execute("INSERT INTO accounts (name, balance) VALUES ('Alice', 1000.0)", [])?;
    conn.execute("INSERT INTO accounts (name, balance) VALUES ('Bob', 500.0)", [])?;

    println!("Initial balances:");
    print_balances(&conn)?;

    // Test successful transaction
    {
        let tx = conn.transaction()?;
        tx.execute("UPDATE accounts SET balance = balance - 200.0 WHERE name = 'Alice'", [])?;
        tx.execute("UPDATE accounts SET balance = balance + 200.0 WHERE name = 'Bob'", [])?;
        tx.commit()?;
    }

    println!("After successful transfer (Alice -> Bob, $200):");
    print_balances(&conn)?;

    // Test failed transaction (rollback)
    let mut transaction_failed = false;
    let result = {
        let tx = conn.transaction()?;
        tx.execute("UPDATE accounts SET balance = balance - 1000.0 WHERE name = 'Bob'", [])?;
        tx.execute("UPDATE accounts SET balance = balance + 1000.0 WHERE name = 'Alice'", [])?;
        
        // Simulate error condition
        if true {
            transaction_failed = true;
            // Just drop the transaction without committing (automatic rollback)
            Ok(())
        } else {
            tx.commit()
        }
    };

    if transaction_failed {
        println!("Transaction failed and was rolled back (as expected)");
    } else {
        println!("Transaction unexpectedly succeeded");
    }

    println!("Balances after failed transaction (should be unchanged):");
    print_balances(&conn)?;

    Ok(())
}

fn print_balances(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("SELECT name, balance FROM accounts ORDER BY name")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
    })?;

    for row in rows {
        let (name, balance) = row?;
        println!("  {}: ${:.2}", name, balance);
    }
    Ok(())
}

fn test_prepared_statements() -> Result<()> {
    println!("\n--- Testing Prepared Statements and Parameters ---");
    let conn = Connection::open_in_memory()?;

    conn.execute(
        "CREATE TABLE products (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            category TEXT NOT NULL,
            price REAL NOT NULL
        )",
        [],
    )?;

    // Insert with named parameters
    conn.execute(
        "INSERT INTO products (name, category, price) VALUES (:name, :category, :price)",
        named_params! {
            ":name": "Laptop",
            ":category": "Electronics",
            ":price": 999.99,
        },
    )?;

    // Insert multiple products with prepared statement
    let mut stmt = conn.prepare("INSERT INTO products (name, category, price) VALUES (?, ?, ?)")?;
    let products = vec![
        ("Mouse", "Electronics", 29.99),
        ("Keyboard", "Electronics", 79.99),
        ("Desk Chair", "Furniture", 199.99),
        ("Monitor", "Electronics", 299.99),
    ];

    for (name, category, price) in products {
        stmt.execute(params![name, category, price])?;
    }

    // Query with parameters
    let category_filter = "Electronics";
    let price_limit = 100.0;

    let mut query_stmt = conn.prepare(
        "SELECT name, price FROM products WHERE category = ?1 AND price < ?2 ORDER BY price"
    )?;
    
    let product_iter = query_stmt.query_map(params![category_filter, price_limit], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
    })?;

    println!("Electronics under ${}:", price_limit);
    for product in product_iter {
        let (name, price) = product?;
        println!("  {}: ${:.2}", name, price);
    }

    Ok(())
}

fn test_joins_and_aggregates() -> Result<()> {
    println!("\n--- Testing Joins and Aggregates ---");
    let conn = Connection::open_in_memory()?;

    // Create related tables
    conn.execute(
        "CREATE TABLE departments (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            budget REAL NOT NULL
        )",
        [],
    )?;

    conn.execute(
        "CREATE TABLE employees (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            department_id INTEGER,
            salary REAL NOT NULL,
            FOREIGN KEY(department_id) REFERENCES departments(id)
        )",
        [],
    )?;

    // Insert departments
    let departments = vec![
        ("Engineering", 1000000.0),
        ("Marketing", 500000.0),
        ("HR", 300000.0),
    ];

    for (i, (name, budget)) in departments.iter().enumerate() {
        conn.execute(
            "INSERT INTO departments (id, name, budget) VALUES (?, ?, ?)",
            params![i + 1, name, budget],
        )?;
    }

    // Insert employees
    let employees = vec![
        ("Alice Engineer", 1, 85000.0),
        ("Bob Engineer", 1, 92000.0),
        ("Charlie Engineer", 1, 78000.0),
        ("Diana Marketing", 2, 65000.0),
        ("Eve Marketing", 2, 72000.0),
        ("Frank HR", 3, 58000.0),
    ];

    for (name, dept_id, salary) in employees {
        conn.execute(
            "INSERT INTO employees (name, department_id, salary) VALUES (?, ?, ?)",
            params![name, dept_id, salary],
        )?;
    }

    // Complex query with JOIN and aggregates
    let mut stmt = conn.prepare(
        "SELECT 
            d.name as department,
            COUNT(e.id) as employee_count,
            AVG(e.salary) as avg_salary,
            MAX(e.salary) as max_salary,
            SUM(e.salary) as total_salary,
            d.budget
         FROM departments d
         LEFT JOIN employees e ON d.id = e.department_id
         GROUP BY d.id, d.name, d.budget
         ORDER BY avg_salary DESC"
    )?;

    let dept_stats = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,      // department
            row.get::<_, i64>(1)?,         // employee_count
            row.get::<_, f64>(2)?,         // avg_salary
            row.get::<_, f64>(3)?,         // max_salary
            row.get::<_, f64>(4)?,         // total_salary
            row.get::<_, f64>(5)?,         // budget
        ))
    })?;

    println!("Department Statistics:");
    for stat in dept_stats {
        let (dept, count, avg, max, total, budget) = stat?;
        println!("  {}: {} employees, avg: ${:.2}, max: ${:.2}, total: ${:.2}, budget: ${:.2}", 
                dept, count, avg, max, total, budget);
    }

    Ok(())
}

fn test_blob_operations() -> Result<()> {
    println!("\n--- Testing BLOB Operations ---");
    let conn = Connection::open_in_memory()?;

    conn.execute(
        "CREATE TABLE files (
            id INTEGER PRIMARY KEY,
            filename TEXT NOT NULL,
            content BLOB,
            size INTEGER
        )",
        [],
    )?;

    // Insert binary data
    let file_data = vec![0u8, 1, 2, 3, 4, 5, 255, 254, 253];
    conn.execute(
        "INSERT INTO files (filename, content, size) VALUES (?, ?, ?)",
        params!["test.bin", &file_data, file_data.len()],
    )?;

    // Insert text as binary
    let text_data = "Hello, SQLite BLOB!".as_bytes();
    conn.execute(
        "INSERT INTO files (filename, content, size) VALUES (?, ?, ?)",
        params!["hello.txt", text_data, text_data.len()],
    )?;

    // Query BLOB data
    let mut stmt = conn.prepare("SELECT filename, content, size FROM files")?;
    let file_iter = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Vec<u8>>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;

    println!("Files in database:");
    for file in file_iter {
        let (filename, content, size) = file?;
        println!("  {}: {} bytes", filename, size);
        
        if filename.ends_with(".txt") {
            if let Ok(text) = String::from_utf8(content) {
                println!("    Content: '{}'", text);
            }
        } else {
            println!("    Binary content: {:?}", &content[..std::cmp::min(content.len(), 10)]);
        }
    }

    Ok(())
}

fn test_user_functions() -> Result<()> {
    println!("\n--- Testing User-Defined Functions ---");
    let conn = Connection::open_in_memory()?;

    // Register a custom function
    conn.create_scalar_function(
        "double",
        1,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8 | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let value = ctx.get::<f64>(0)?;
            Ok(value * 2.0)
        },
    )?;

    // Register another custom function
    conn.create_scalar_function(
        "concat_with_prefix",
        2,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8 | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let prefix = ctx.get::<String>(0)?;
            let text = ctx.get::<String>(1)?;
            Ok(format!("{}: {}", prefix, text))
        },
    )?;

    conn.execute(
        "CREATE TABLE numbers (
            id INTEGER PRIMARY KEY,
            value REAL,
            name TEXT
        )",
        [],
    )?;

    // Insert test data
    let test_data = vec![
        (3.14, "pi"),
        (2.718, "e"),
        (1.414, "sqrt2"),
        (1.618, "phi"),
    ];

    for (value, name) in test_data {
        conn.execute(
            "INSERT INTO numbers (value, name) VALUES (?, ?)",
            params![value, name],
        )?;
    }

    // Use custom functions in queries
    let mut stmt = conn.prepare(
        "SELECT 
            name,
            value,
            double(value) as doubled,
            concat_with_prefix('Number', name) as prefixed_name
         FROM numbers 
         ORDER BY value"
    )?;

    let results = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, f64>(1)?,
            row.get::<_, f64>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;

    println!("Using custom functions:");
    for result in results {
        let (name, value, doubled, prefixed) = result?;
        println!("  {}: {:.3} -> doubled: {:.3}, prefixed: '{}'", 
                name, value, doubled, prefixed);
    }

    Ok(())
}

fn test_persistent_data() -> Result<()> {
    println!("\n--- Testing Persistent Data ---");
    
    let db_path = "test_persistent.db";
    
    // Phase 1: Create database and insert data
    {
        println!("Phase 1: Creating database and inserting data...");
        let conn = Connection::open(db_path)?;
        
        conn.execute(
            "CREATE TABLE IF NOT EXISTS persistent_test (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                value INTEGER NOT NULL,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )",
            [],
        )?;
        
        // Insert test data
        let test_data = vec![
            ("First Record", 100),
            ("Second Record", 200),
            ("Third Record", 300),
        ];
        
        for (name, value) in test_data {
            conn.execute(
                "INSERT INTO persistent_test (name, value) VALUES (?, ?)",
                params![name, value],
            )?;
        }
        
        // Verify data was inserted
        let mut stmt = conn.prepare("SELECT COUNT(*) FROM persistent_test")?;
        let count: i64 = stmt.query_row([], |row| row.get(0))?;
        println!("  Inserted {} records", count);
        
        // Connection automatically closes when going out of scope
    }
    
    // Phase 2: Reopen database and verify data persists
    {
        println!("Phase 2: Reopening database and verifying data...");
        let conn = Connection::open(db_path)?;
        
        // Query the data
        let mut stmt = conn.prepare("SELECT id, name, value FROM persistent_test ORDER BY id")?;
        let records = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,    // id
                row.get::<_, String>(1)?, // name
                row.get::<_, i64>(2)?,    // value
            ))
        })?;
        
        println!("  Records found in reopened database:");
        let mut record_count = 0;
        for record in records {
            let (id, name, value) = record?;
            println!("    ID: {}, Name: '{}', Value: {}", id, name, value);
            record_count += 1;
        }
        
        if record_count == 0 {
            return Err(rusqlite::Error::InvalidColumnName("No persistent data found!".to_string()));
        }
        
        println!("  Successfully verified {} persistent records", record_count);
    }
    
    // Phase 3: Update data and verify persistence
    {
        println!("Phase 3: Updating data and testing persistence...");
        let conn = Connection::open(db_path)?;
        
        // Update some records
        conn.execute(
            "UPDATE persistent_test SET value = value + 1000 WHERE id <= 2",
            [],
        )?;
        
        // Add a new record
        conn.execute(
            "INSERT INTO persistent_test (name, value) VALUES (?, ?)",
            params!["Fourth Record", 400],
        )?;
        
        println!("  Updated existing records and added new record");
    }
    
    // Phase 4: Final verification
    {
        println!("Phase 4: Final verification of all changes...");
        let conn = Connection::open(db_path)?;
        
        let mut stmt = conn.prepare(
            "SELECT id, name, value FROM persistent_test ORDER BY id"
        )?;
        let records = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        
        println!("  Final state of persistent database:");
        for record in records {
            let (id, name, value) = record?;
            println!("    ID: {}, Name: '{}', Value: {}", id, name, value);
        }
        
        // Get total count
        let mut count_stmt = conn.prepare("SELECT COUNT(*) FROM persistent_test")?;
        let total_count: i64 = count_stmt.query_row([], |row| row.get(0))?;
        println!("  Total persistent records: {}", total_count);
    }
    
    // Cleanup (optional - remove test database file)
    if std::path::Path::new(db_path).exists() {
        std::fs::remove_file(db_path).unwrap_or_else(|e| {
            println!("  Warning: Could not remove test database file: {}", e);
        });
        println!("  Cleaned up test database file");
    }
    
    println!("  ✓ Persistent data test completed successfully!");
    Ok(())
}