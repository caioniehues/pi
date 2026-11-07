//! The portable SQLite interface (TS `sqlite/database.ts`, `sqlite/index.ts`): values, rows, the executor and
//! database traits Durable's sqlite storage is written against, and their errors. Migrations and schema prefixes are
//! `migrations.rs`; the driver over `rusqlite` is `rusqlite.rs`.

mod migrations;
mod rusqlite;

pub use self::migrations::{SqliteMigration, apply_sqlite_migrations, validate_sqlite_schema_prefix};
pub use self::rusqlite::{RusqliteDatabase, RusqliteOptions, RusqliteTransaction, SqliteMode, open_rusqlite_database};
use std::fmt::Display;
use std::future::Future;
use std::sync::Arc;

/// A value SQLite stores or binds (TS `SqliteValue`; a JS number is `Integer` or `Real` here, as SQLite types it).
#[derive(Clone, Debug, PartialEq)]
pub enum SqliteValue {
	/// `NULL`.
	Null,
	/// A 64-bit integer.
	Integer(i64),
	/// A double.
	Real(f64),
	/// UTF-8 text.
	Text(String),
	/// Bytes.
	Blob(Vec<u8>),
}

/// One result row: its column names (shared by the rows of one statement) and values.
#[derive(Clone, Debug, PartialEq)]
pub struct SqliteRow {
	columns: Arc<[String]>,
	values: Vec<SqliteValue>,
}

/// Why a SQLite operation failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SqliteError {
	/// The SQLite engine refused, with its message.
	#[error("{0}")]
	Sqlite(String),
	/// A transaction handle used after its transaction settled.
	#[error("SQLite transaction handle is no longer active")]
	InactiveTransaction,
	/// The database was closed.
	#[error("database is not open")]
	Closed,
	/// The transaction failed (the callback or `COMMIT`) and its `ROLLBACK` failed too (TS `AggregateError`): the
	/// caller cannot tell whether anything was committed.
	#[error("SQLite transaction failed and rollback failed: {error}; {rollback}")]
	RollbackFailed {
		/// The callback's or `COMMIT`'s error.
		error: String,
		/// The rollback's error.
		rollback: String,
	},
	/// Migration versions are not 1, 2, 3, ... (TS message).
	#[error("SQLite migrations must have contiguous versions starting at 1")]
	MigrationVersions,
	/// The metadata row of a migration table is missing.
	#[error("SQLite schema metadata is missing")]
	MissingSchemaMetadata,
	/// The database was migrated by a newer program.
	#[error("SQLite schema version {found} is newer than supported version {supported}")]
	SchemaTooNew {
		/// The version stored.
		found: i64,
		/// The newest version this program knows.
		supported: u32,
	},
	/// A schema prefix that is not an identifier (the TS message quotes it as JSON).
	#[error("Invalid SQLite schema prefix {0}")]
	InvalidPrefix(String),
	/// A schema prefix SQLite reserves (`sqlite_...`).
	#[error("SQLite schema prefix {0} is reserved")]
	ReservedPrefix(String),
}

/// SQL operations shared by a database and its transaction handles (TS `SqliteExecutor`).
///
/// `exec` runs SQL text without bindings and may hold several statements. `run`, `get` and `all` run one statement
/// with positional bindings. `run_counted` runs one like `run` and resolves with the rows it inserted, updated or
/// deleted, as SQLite's `changes()` counts them. Drivers may cache prepared statements by SQL text, so callers bind
/// values instead of interpolating them.
pub trait SqliteExecutor: Send + Sync {
	/// Runs SQL text, which may hold several statements.
	fn exec(&self, sql: &str) -> impl Future<Output = Result<(), SqliteError>> + Send;

	/// Runs one statement.
	fn run(&self, sql: &str, params: &[SqliteValue]) -> impl Future<Output = Result<(), SqliteError>> + Send;

	/// Runs one statement and resolves with the rows it changed.
	fn run_counted(&self, sql: &str, params: &[SqliteValue]) -> impl Future<Output = Result<u64, SqliteError>> + Send;

	/// The first row of one statement.
	fn get(
		&self,
		sql: &str,
		params: &[SqliteValue],
	) -> impl Future<Output = Result<Option<SqliteRow>, SqliteError>> + Send;

	/// Every row of one statement.
	fn all(
		&self,
		sql: &str,
		params: &[SqliteValue],
	) -> impl Future<Output = Result<Vec<SqliteRow>, SqliteError>> + Send;
}

/// A SQLite database (TS `SqliteDatabase`).
///
/// `transaction` passes the callback a transaction handle; all work of the transaction goes through it, and it is
/// inactive once the callback settled. Drivers queue unrelated operations and other transactions until it finishes,
/// so calling the database itself from inside a callback, and awaiting that call there, never settles. When the
/// callback fails, the driver rolls back before failing with that same error; when the rollback fails too, it fails
/// with `SqliteError::RollbackFailed` instead, so a caller cannot mistake the callback's error for a rollback.
pub trait SqliteDatabase: SqliteExecutor {
	/// The handle a transaction callback gets.
	type Transaction: SqliteExecutor + 'static;

	/// Runs `callback` in one transaction: commits when it succeeds, rolls back when it fails.
	fn transaction<T, E, F, Fut>(&self, callback: F) -> impl Future<Output = Result<T, E>> + Send
	where
		F: FnOnce(Self::Transaction) -> Fut + Send,
		Fut: Future<Output = Result<T, E>> + Send,
		T: Send,
		E: From<SqliteError> + Display + Send;

	/// Closes the database; idempotent.
	fn close(&self) -> impl Future<Output = Result<(), SqliteError>> + Send;
}

impl SqliteRow {
	/// A row of `values` under `columns`.
	pub fn new(columns: Arc<[String]>, values: Vec<SqliteValue>) -> Self {
		Self { columns, values }
	}

	/// The value of column `name`.
	pub fn get(&self, name: &str) -> Option<&SqliteValue> {
		let position = self.columns.iter().position(|column| column == name)?;
		self.values.get(position)
	}

	/// The column names, in result order.
	pub fn columns(&self) -> &[String] {
		&self.columns
	}

	/// The values, in result order.
	pub fn values(&self) -> &[SqliteValue] {
		&self.values
	}
}
