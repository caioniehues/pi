//! Schema migrations and the identifiers they use (TS `sqlite/migrations.ts`, `sqlite/schema.ts`): versioned
//! statement lists applied atomically, tracked in a metadata table named by the caller.

use crate::sqlite::{SqliteDatabase, SqliteError, SqliteExecutor, SqliteValue};
use pi_json::{JsonValue, to_js_string};

/// One schema version: the statements that move the previous version to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SqliteMigration {
	/// The version, 1 for the first migration.
	pub version: u32,
	/// The statements, run in order.
	pub statements: Vec<String>,
}

/// Applies all pending migrations in one transaction, tracking the version in the metadata table `table`; nothing
/// runs when a version or the table name is invalid, and a failing statement rolls every pending one back.
pub async fn apply_sqlite_migrations<D: SqliteDatabase>(
	database: &D,
	migrations: &[SqliteMigration],
	table: &str,
) -> Result<(), SqliteError> {
	validate_sqlite_schema_prefix(table)?;
	if migrations
		.iter()
		.enumerate()
		.any(|(index, migration)| u32::try_from(index + 1) != Ok(migration.version))
	{
		return Err(SqliteError::MigrationVersions);
	}
	let current = migrations.last().map_or(0, |migration| migration.version);
	database
		.transaction(|transaction| async move { migrate(&transaction, migrations, table, current).await })
		.await
}

async fn migrate<X: SqliteExecutor>(
	transaction: &X,
	migrations: &[SqliteMigration],
	table: &str,
	current: u32,
) -> Result<(), SqliteError> {
	// The table name was validated as an identifier: interpolating it is safe.
	transaction
		.exec(&format!(
			"CREATE TABLE IF NOT EXISTS {table} (\n\t\t\tsingleton INTEGER PRIMARY KEY CHECK (singleton = 1),\n\t\t\t\
			 version INTEGER NOT NULL CHECK (version >= 0)\n\t\t) STRICT"
		))
		.await?;
	transaction
		.run(
			&format!("INSERT OR IGNORE INTO {table} (singleton, version) VALUES (1, 0)"),
			&[],
		)
		.await?;
	let row = transaction
		.get(&format!("SELECT version FROM {table} WHERE singleton = 1"), &[])
		.await?;
	let Some(SqliteValue::Integer(found)) = row.as_ref().and_then(|row| row.get("version")).cloned() else {
		return Err(SqliteError::MissingSchemaMetadata);
	};
	if found > i64::from(current) {
		return Err(SqliteError::SchemaTooNew {
			found,
			supported: current,
		});
	}
	for migration in migrations
		.iter()
		.filter(|migration| i64::from(migration.version) > found)
	{
		for statement in &migration.statements {
			transaction.exec(statement).await?;
		}
		let version = SqliteValue::Integer(i64::from(migration.version));
		transaction
			.run(
				&format!("UPDATE {table} SET version = ? WHERE singleton = 1"),
				&[version],
			)
			.await?;
	}
	Ok(())
}

/// Checks that `prefix` can start a SQLite identifier: ASCII letters, digits and `_`, not starting with a digit, and
/// not SQLite's reserved `sqlite_`.
pub fn validate_sqlite_schema_prefix(prefix: &str) -> Result<(), SqliteError> {
	let quoted = || to_js_string(&JsonValue::String(prefix.to_owned()));
	let mut bytes = prefix.bytes();
	let starts = bytes
		.next()
		.is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_');
	if !starts || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
		return Err(SqliteError::InvalidPrefix(quoted()));
	}
	if prefix.to_ascii_lowercase().starts_with("sqlite_") {
		return Err(SqliteError::ReservedPrefix(quoted()));
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::{SqliteMigration, apply_sqlite_migrations, validate_sqlite_schema_prefix};
	use crate::sqlite::{RusqliteDatabase, RusqliteOptions, SqliteExecutor, SqliteValue, open_rusqlite_database};
	use crate::test_executor::now;

	fn ready<T>(future: impl std::future::Future<Output = T>) -> T {
		now(future).unwrap()
	}

	fn memory() -> RusqliteDatabase {
		ready(open_rusqlite_database(":memory:", RusqliteOptions::default())).unwrap()
	}

	fn migration(version: u32, statements: &[&str]) -> SqliteMigration {
		SqliteMigration {
			version,
			statements: statements.iter().map(|statement| (*statement).to_owned()).collect(),
		}
	}

	fn version(database: &RusqliteDatabase, table: &str) -> Option<SqliteValue> {
		let row = ready(database.get(&format!("SELECT version FROM {table}"), &[])).unwrap();
		row.and_then(|row| row.get("version").cloned())
	}

	fn schema_names(database: &RusqliteDatabase) -> usize {
		ready(database.all("SELECT name FROM sqlite_schema", &[]))
			.unwrap()
			.len()
	}

	// TS: sqlite/migrations.test.ts > SQLite migrations > tracks independent histories and skips migrations already
	// applied
	#[test]
	fn tracks_independent_histories_and_skips_migrations_already_applied() {
		let database = memory();
		let first = [migration(
			1,
			&["CREATE TABLE notes (body TEXT)", "INSERT INTO notes VALUES ('kept')"],
		)];
		ready(apply_sqlite_migrations(&database, &first, "notes_schema")).unwrap();
		ready(apply_sqlite_migrations(&database, &first, "notes_schema")).unwrap();
		ready(apply_sqlite_migrations(&database, &[], "other_schema")).unwrap();
		let second = [
			first[0].clone(),
			migration(2, &["ALTER TABLE notes ADD COLUMN title TEXT"]),
		];
		ready(apply_sqlite_migrations(&database, &second, "notes_schema")).unwrap();
		let rows = ready(database.all("SELECT body, title FROM notes", &[])).unwrap();
		assert_eq!(rows.len(), 1);
		assert_eq!(
			rows[0].values(),
			[SqliteValue::Text("kept".to_owned()), SqliteValue::Null]
		);
		assert_eq!(version(&database, "notes_schema"), Some(SqliteValue::Integer(2)));
		assert_eq!(version(&database, "other_schema"), Some(SqliteValue::Integer(0)));
		let error = ready(apply_sqlite_migrations(&database, &first, "notes_schema")).unwrap_err();
		assert_eq!(
			error.to_string(),
			"SQLite schema version 2 is newer than supported version 1"
		);
	}

	// TS: sqlite/migrations.test.ts > SQLite migrations > rejects invalid migration versions before touching the database
	#[test]
	fn rejects_invalid_migration_versions_before_touching_the_database() {
		let database = memory();
		for versions in [&[0][..], &[2], &[1, 3], &[1, 1]] {
			let migrations: Vec<SqliteMigration> = versions.iter().map(|&version| migration(version, &[])).collect();
			let error = ready(apply_sqlite_migrations(&database, &migrations, "app_schema")).unwrap_err();
			assert!(error.to_string().contains("contiguous versions starting at 1"));
		}
		assert_eq!(schema_names(&database), 0);
	}

	// TS: sqlite/migrations.test.ts > SQLite migrations > validates prefixes and migration table identifiers before
	// executing SQL
	#[test]
	fn validates_prefixes_and_migration_table_identifiers_before_executing_sql() {
		let database = memory();
		for prefix in [
			"",
			"1bad",
			"bad-prefix",
			"bad; DROP TABLE notes",
			"sqlite_app",
			"SQLITE_app",
		] {
			assert!(validate_sqlite_schema_prefix(prefix).is_err(), "{prefix}");
			assert!(
				ready(apply_sqlite_migrations(&database, &[], prefix)).is_err(),
				"{prefix}"
			);
		}
		for prefix in ["pi_", "_private", "app2"] {
			validate_sqlite_schema_prefix(prefix).unwrap();
		}
		assert_eq!(
			validate_sqlite_schema_prefix("bad-prefix").unwrap_err().to_string(),
			r#"Invalid SQLite schema prefix "bad-prefix""#
		);
		assert_eq!(
			validate_sqlite_schema_prefix("sqlite_app").unwrap_err().to_string(),
			r#"SQLite schema prefix "sqlite_app" is reserved"#
		);
		assert_eq!(schema_names(&database), 0);
	}

	// TS: sqlite/migrations.test.ts > SQLite migrations > rolls initial bootstrap and every pending migration back
	// together
	#[test]
	fn rolls_initial_bootstrap_and_every_pending_migration_back_together() {
		let database = memory();
		let failed = [
			migration(
				1,
				&[
					"CREATE TABLE migration_first (value TEXT) STRICT",
					"INSERT INTO migration_first (value) VALUES ('retained')",
				],
			),
			migration(
				2,
				&["CREATE TABLE migration_second (value TEXT) STRICT", "THIS IS NOT SQL"],
			),
		];
		assert!(ready(apply_sqlite_migrations(&database, &failed, "app_schema")).is_err());
		let count = ready(database.get(
			"SELECT count(*) AS count FROM sqlite_schema WHERE name IN ('app_schema', 'migration_first', 'migration_second')",
			&[],
		))
		.unwrap();
		assert_eq!(
			count.and_then(|row| row.get("count").cloned()),
			Some(SqliteValue::Integer(0))
		);
		let fixed = [
			failed[0].clone(),
			migration(2, &["CREATE TABLE migration_second (value TEXT) STRICT"]),
		];
		ready(apply_sqlite_migrations(&database, &fixed, "app_schema")).unwrap();
		assert_eq!(version(&database, "app_schema"), Some(SqliteValue::Integer(2)));
		let value = ready(database.get("SELECT value FROM migration_first", &[])).unwrap();
		assert_eq!(
			value.and_then(|row| row.get("value").cloned()),
			Some(SqliteValue::Text("retained".to_owned()))
		);
	}
}
