//! The SQLite driver over `rusqlite` with SQLite compiled in (TS `sqlite/node.ts`, the `node:sqlite` driver): the
//! same pragmas, modes, busy timeout, statement cache and transaction semantics.
//!
//! Settings, each mirroring `packages/chord/src/sqlite/node.ts` or the `DatabaseSync` defaults it relies on:
//! - every connection: busy timeout (`new DatabaseSync(path, { timeout })`, node.ts:215 and 217, default 5,000 ms);
//!   foreign keys on (`PRAGMA foreign_keys = ON`, Node's `enableForeignKeyConstraints: true` default, set explicitly
//!   rather than trusting the bundled build's default); double-quoted string literals refused in DML and DDL
//!   (`SQLITE_DBCONFIG_DQS_DML`, `SQLITE_DBCONFIG_DQS_DDL` off, Node's `enableDoubleQuotedStringLiterals: false`);
//!   a 256-statement LRU of prepared statements (node.ts `STATEMENT_CACHE_LIMIT`).
//! - read-only: opened with `SQLITE_OPEN_READ_ONLY` (`readOnly: true`, node.ts:215); nothing else.
//! - read-write: the parent directory created (node.ts:216), `PRAGMA journal_mode = WAL` (node.ts:220),
//!   `PRAGMA synchronous = NORMAL` (node.ts:221), `PRAGMA wal_autocheckpoint = <pages>` (node.ts:222), and
//!   `PRAGMA wal_checkpoint(TRUNCATE)` on close (node.ts:195).
//! - transactions: `BEGIN IMMEDIATE`, or `BEGIN` read-only (node.ts:170), then `COMMIT` (node.ts:175) or `ROLLBACK`
//!   (node.ts:180). A transaction future dropped before it settled rolls back while it still holds its turn, so the
//!   connection is never left inside a transaction.
//!
//! As `node:sqlite`, the engine is synchronous: each operation runs on the task that polls it, inside its turn of the
//! connection's queue. Operations take their turn when they are called, not when first polled, so they run in call
//! order whether they start at once or wait (TS `SerialOperationQueue`).

use crate::sqlite::{SqliteDatabase, SqliteError, SqliteExecutor, SqliteRow, SqliteValue};
use ::rusqlite::config::DbConfig;
use ::rusqlite::types::{ToSql, ToSqlOutput, ValueRef};
use ::rusqlite::{Connection, OpenFlags, params_from_iter};
use std::collections::{HashMap, HashSet};
use std::fmt::Display;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context as TaskContext, Poll, Waker};
use std::time::Duration;

/// How a connection opens its file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SqliteMode {
	/// Creates the file and its directory as needed and configures WAL.
	#[default]
	ReadWrite,
	/// Opens an existing file for reads (another process may write it): nothing is made, configured or checkpointed,
	/// and transactions are deferred (`BEGIN`), reading one snapshot.
	ReadOnly,
}

/// Connection settings for a database file (TS `NodeSqliteDatabaseOptions`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RusqliteOptions {
	/// How the file is opened.
	pub mode: SqliteMode,
	/// The WAL auto-checkpoint threshold in pages; 0 disables it. SQLite and this driver default to 1,000.
	pub wal_auto_checkpoint_pages: u32,
	/// How long SQLite waits for a competing file lock. SQLite defaults to 0; this driver to 5,000 ms.
	pub busy_timeout_ms: u64,
}

/// A database on one `rusqlite` connection; clones share it.
#[derive(Clone)]
pub struct RusqliteDatabase {
	shared: Arc<Shared>,
}

/// The handle of one transaction; inactive once its callback settled.
pub struct RusqliteTransaction {
	shared: Arc<Shared>,
	active: Arc<AtomicBool>,
}

struct Shared {
	/// `None` once closed.
	connection: Mutex<Option<Connection>>,
	queue: Arc<SerialQueue>,
	mode: SqliteMode,
}

impl Default for RusqliteOptions {
	fn default() -> Self {
		Self {
			mode: SqliteMode::ReadWrite,
			wal_auto_checkpoint_pages: 1_000,
			busy_timeout_ms: 5_000,
		}
	}
}

/// Prepared statements one connection keeps, least recently used evicted first (TS `STATEMENT_CACHE_LIMIT`).
const STATEMENT_CACHE_LIMIT: usize = 256;

/// Opens and configures a database at `path` (`":memory:"` for an in-memory one).
pub async fn open_rusqlite_database(
	path: impl AsRef<Path>,
	options: RusqliteOptions,
) -> Result<RusqliteDatabase, SqliteError> {
	let path = path.as_ref();
	let timeout = Duration::from_millis(options.busy_timeout_ms);
	if options.mode == SqliteMode::ReadOnly {
		let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX;
		let connection = Connection::open_with_flags(path, flags).map_err(engine)?;
		configure(&connection, timeout)?;
		return Ok(RusqliteDatabase::new(connection, SqliteMode::ReadOnly));
	}
	if path != Path::new(":memory:")
		&& let Some(directory) = path.parent().filter(|directory| !directory.as_os_str().is_empty())
	{
		std::fs::create_dir_all(directory).map_err(|error| SqliteError::Sqlite(error.to_string()))?;
	}
	let connection = Connection::open(path).map_err(engine)?;
	configure(&connection, timeout)?;
	let database = RusqliteDatabase::new(connection, SqliteMode::ReadWrite);
	let configured = async {
		database.exec("PRAGMA journal_mode = WAL").await?;
		database.exec("PRAGMA synchronous = NORMAL").await?;
		let pages = options.wal_auto_checkpoint_pages;
		database.exec(&format!("PRAGMA wal_autocheckpoint = {pages}")).await
	};
	if let Err(error) = configured.await {
		// The configuration failure is the error to report; a failing close would only hide it.
		let _closed = database.close().await;
		return Err(error);
	}
	Ok(database)
}

/// The settings of every connection, read-only ones included: what `node:sqlite` sets by default.
fn configure(connection: &Connection, timeout: Duration) -> Result<(), SqliteError> {
	connection.busy_timeout(timeout).map_err(engine)?;
	connection
		.set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DML, false)
		.map_err(engine)?;
	connection
		.set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DDL, false)
		.map_err(engine)?;
	connection.execute_batch("PRAGMA foreign_keys = ON").map_err(engine)
}

/// The engine's error as a `SqliteError`, keeping its message.
#[expect(clippy::needless_pass_by_value, reason = "the shape `map_err` takes")]
fn engine(error: ::rusqlite::Error) -> SqliteError {
	SqliteError::Sqlite(error.to_string())
}

impl RusqliteDatabase {
	fn new(connection: Connection, mode: SqliteMode) -> Self {
		connection.set_prepared_statement_cache_capacity(STATEMENT_CACHE_LIMIT);
		Self {
			shared: Arc::new(Shared {
				connection: Mutex::new(Some(connection)),
				queue: Arc::new(SerialQueue::default()),
				mode,
			}),
		}
	}

	/// Runs `operation` on the connection in its turn of the queue, taken now.
	fn queued<T: Send>(
		&self,
		operation: impl FnOnce(&Connection) -> Result<T, SqliteError> + Send,
	) -> impl Future<Output = Result<T, SqliteError>> + Send {
		let mut turn = self.shared.queue.turn();
		let shared = self.shared.clone();
		async move {
			(&mut turn).await;
			shared.with(operation)
		}
	}
}

impl Shared {
	fn lock(&self) -> MutexGuard<'_, Option<Connection>> {
		self.connection.lock().unwrap_or_else(PoisonError::into_inner)
	}

	fn with<T>(&self, operation: impl FnOnce(&Connection) -> Result<T, SqliteError>) -> Result<T, SqliteError> {
		match &*self.lock() {
			Some(connection) => operation(connection),
			None => Err(SqliteError::Closed),
		}
	}

	/// Rolls back after `error` (the callback's or `COMMIT`'s); a failing rollback replaces it (TS `AggregateError`).
	fn rollback_after<E: From<SqliteError> + Display>(&self, error: E) -> E {
		match self.with(|connection| exec(connection, "ROLLBACK")) {
			Ok(()) => error,
			Err(rollback) => E::from(SqliteError::RollbackFailed {
				error: error.to_string(),
				rollback: rollback.to_string(),
			}),
		}
	}
}

impl SqliteExecutor for RusqliteDatabase {
	fn exec(&self, sql: &str) -> impl Future<Output = Result<(), SqliteError>> + Send {
		self.queued(move |connection| exec(connection, sql))
	}

	fn run(&self, sql: &str, params: &[SqliteValue]) -> impl Future<Output = Result<(), SqliteError>> + Send {
		self.queued(move |connection| run(connection, sql, params).map(drop))
	}

	fn run_counted(&self, sql: &str, params: &[SqliteValue]) -> impl Future<Output = Result<u64, SqliteError>> + Send {
		self.queued(move |connection| run(connection, sql, params))
	}

	fn get(
		&self,
		sql: &str,
		params: &[SqliteValue],
	) -> impl Future<Output = Result<Option<SqliteRow>, SqliteError>> + Send {
		self.queued(move |connection| first_row(connection, sql, params))
	}

	fn all(
		&self,
		sql: &str,
		params: &[SqliteValue],
	) -> impl Future<Output = Result<Vec<SqliteRow>, SqliteError>> + Send {
		self.queued(move |connection| all_rows(connection, sql, params))
	}
}

impl SqliteDatabase for RusqliteDatabase {
	type Transaction = RusqliteTransaction;

	fn transaction<T, E, F, Fut>(&self, callback: F) -> impl Future<Output = Result<T, E>> + Send
	where
		F: FnOnce(RusqliteTransaction) -> Fut + Send,
		Fut: Future<Output = Result<T, E>> + Send,
		T: Send,
		E: From<SqliteError> + Display + Send,
	{
		let mut turn = self.shared.queue.turn();
		let shared = self.shared.clone();
		async move {
			(&mut turn).await;
			// A writer takes the write lock at once; a read-only connection cannot, and reads one snapshot.
			let begin = match shared.mode {
				SqliteMode::ReadOnly => "BEGIN",
				SqliteMode::ReadWrite => "BEGIN IMMEDIATE",
			};
			shared.with(|connection| exec(connection, begin))?;
			let active = Arc::new(AtomicBool::new(true));
			// Declared after the turn, so a drop rolls back before the turn passes on.
			let mut open = OpenTransaction {
				shared: shared.clone(),
				active: active.clone(),
				settled: false,
			};
			let handle = RusqliteTransaction {
				shared: shared.clone(),
				active,
			};
			let result = callback(handle).await;
			open.settle();
			let outcome = match result {
				Ok(value) => match shared.with(|connection| exec(connection, "COMMIT")) {
					Ok(()) => Ok(value),
					Err(commit) => Err(shared.rollback_after(E::from(commit))),
				},
				Err(error) => Err(shared.rollback_after(error)),
			};
			drop(turn);
			outcome
		}
	}

	fn close(&self) -> impl Future<Output = Result<(), SqliteError>> + Send {
		let mut turn = self.shared.queue.turn();
		let shared = self.shared.clone();
		async move {
			(&mut turn).await;
			let Some(connection) = shared.lock().take() else {
				return Ok(());
			};
			let checkpoint = match shared.mode {
				SqliteMode::ReadWrite => exec(&connection, "PRAGMA wal_checkpoint(TRUNCATE)"),
				SqliteMode::ReadOnly => Ok(()),
			};
			let closed = connection.close().map_err(|(_, error)| engine(error));
			checkpoint.and(closed)
		}
	}
}

/// A transaction between `BEGIN` and its settling: dropped unsettled (its future was dropped mid-callback), it
/// deactivates the handle and rolls back.
struct OpenTransaction {
	shared: Arc<Shared>,
	active: Arc<AtomicBool>,
	settled: bool,
}

impl OpenTransaction {
	/// The callback settled: the handle is inactive, and the caller commits or rolls back.
	fn settle(&mut self) {
		self.active.store(false, Ordering::SeqCst);
		self.settled = true;
	}
}

impl Drop for OpenTransaction {
	fn drop(&mut self) {
		if self.settled {
			return;
		}
		self.active.store(false, Ordering::SeqCst);
		// Nobody is left to report a failing rollback to; a connection that cannot roll back fails its next call.
		let _rolled_back = self.shared.with(|connection| exec(connection, "ROLLBACK"));
	}
}

impl RusqliteTransaction {
	/// Runs `operation` on the connection, which the transaction holds the turn of, while it is active.
	fn active<T>(&self, operation: impl FnOnce(&Connection) -> Result<T, SqliteError>) -> Result<T, SqliteError> {
		if !self.active.load(Ordering::SeqCst) {
			return Err(SqliteError::InactiveTransaction);
		}
		self.shared.with(operation)
	}
}

impl SqliteExecutor for RusqliteTransaction {
	fn exec(&self, sql: &str) -> impl Future<Output = Result<(), SqliteError>> + Send {
		let result = self.active(|connection| exec(connection, sql));
		async move { result }
	}

	fn run(&self, sql: &str, params: &[SqliteValue]) -> impl Future<Output = Result<(), SqliteError>> + Send {
		let result = self.active(|connection| run(connection, sql, params).map(drop));
		async move { result }
	}

	fn run_counted(&self, sql: &str, params: &[SqliteValue]) -> impl Future<Output = Result<u64, SqliteError>> + Send {
		let result = self.active(|connection| run(connection, sql, params));
		async move { result }
	}

	fn get(
		&self,
		sql: &str,
		params: &[SqliteValue],
	) -> impl Future<Output = Result<Option<SqliteRow>, SqliteError>> + Send {
		let result = self.active(|connection| first_row(connection, sql, params));
		async move { result }
	}

	fn all(
		&self,
		sql: &str,
		params: &[SqliteValue],
	) -> impl Future<Output = Result<Vec<SqliteRow>, SqliteError>> + Send {
		let result = self.active(|connection| all_rows(connection, sql, params));
		async move { result }
	}
}

fn exec(connection: &Connection, sql: &str) -> Result<(), SqliteError> {
	connection.execute_batch(sql).map_err(engine)
}

/// Runs one statement to completion (rows it returns are read and dropped, as `node:sqlite`'s `run` does) and
/// returns `changes()`.
fn run(connection: &Connection, sql: &str, params: &[SqliteValue]) -> Result<u64, SqliteError> {
	{
		let mut statement = connection.prepare_cached(sql).map_err(engine)?;
		let mut rows = statement.query(params_from_iter(params)).map_err(engine)?;
		while rows.next().map_err(engine)?.is_some() {}
	}
	Ok(connection.changes())
}

fn first_row(connection: &Connection, sql: &str, params: &[SqliteValue]) -> Result<Option<SqliteRow>, SqliteError> {
	Ok(query(connection, sql, params, Some(1))?.into_iter().next())
}

fn all_rows(connection: &Connection, sql: &str, params: &[SqliteValue]) -> Result<Vec<SqliteRow>, SqliteError> {
	query(connection, sql, params, None)
}

/// The rows of one statement, at most `limit`.
fn query(
	connection: &Connection,
	sql: &str,
	params: &[SqliteValue],
	limit: Option<usize>,
) -> Result<Vec<SqliteRow>, SqliteError> {
	let mut statement = connection.prepare_cached(sql).map_err(engine)?;
	let columns: Arc<[String]> = statement.column_names().into_iter().map(str::to_owned).collect();
	let mut rows = statement.query(params_from_iter(params)).map_err(engine)?;
	let mut out = Vec::new();
	while limit.is_none_or(|limit| out.len() < limit)
		&& let Some(row) = rows.next().map_err(engine)?
	{
		let values = (0..columns.len())
			.map(|index| row.get_ref(index).map(value).map_err(engine))
			.collect::<Result<_, _>>()?;
		out.push(SqliteRow::new(columns.clone(), values));
	}
	Ok(out)
}

fn value(value: ValueRef<'_>) -> SqliteValue {
	match value {
		ValueRef::Null => SqliteValue::Null,
		ValueRef::Integer(integer) => SqliteValue::Integer(integer),
		ValueRef::Real(real) => SqliteValue::Real(real),
		ValueRef::Text(text) => SqliteValue::Text(String::from_utf8_lossy(text).into_owned()),
		ValueRef::Blob(bytes) => SqliteValue::Blob(bytes.to_vec()),
	}
}

impl ToSql for SqliteValue {
	fn to_sql(&self) -> ::rusqlite::Result<ToSqlOutput<'_>> {
		Ok(ToSqlOutput::Borrowed(match self {
			SqliteValue::Null => ValueRef::Null,
			SqliteValue::Integer(integer) => ValueRef::Integer(*integer),
			SqliteValue::Real(real) => ValueRef::Real(*real),
			SqliteValue::Text(text) => ValueRef::Text(text.as_bytes()),
			SqliteValue::Blob(bytes) => ValueRef::Blob(bytes),
		}))
	}
}

/// Turns of one connection, handed out in call order (TS `SerialOperationQueue`).
#[derive(Default)]
struct SerialQueue {
	state: Mutex<QueueState>,
}

#[derive(Default)]
struct QueueState {
	next: u64,
	serving: u64,
	waiting: HashMap<u64, Waker>,
	/// Turns whose futures were dropped before their turn came: skipped.
	abandoned: HashSet<u64>,
}

/// One turn: resolves when it comes, and holds it until dropped.
struct Turn {
	queue: Arc<SerialQueue>,
	ticket: u64,
}

impl SerialQueue {
	fn lock(&self) -> MutexGuard<'_, QueueState> {
		self.state.lock().unwrap_or_else(PoisonError::into_inner)
	}

	fn turn(self: &Arc<Self>) -> Turn {
		let mut state = self.lock();
		let ticket = state.next;
		state.next += 1;
		Turn {
			queue: self.clone(),
			ticket,
		}
	}
}

impl Future for Turn {
	type Output = ();

	fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<()> {
		let mut state = self.queue.lock();
		if state.serving == self.ticket {
			return Poll::Ready(());
		}
		state.waiting.insert(self.ticket, cx.waker().clone());
		Poll::Pending
	}
}

impl Drop for Turn {
	fn drop(&mut self) {
		let mut state = self.queue.lock();
		state.waiting.remove(&self.ticket);
		if state.serving != self.ticket {
			state.abandoned.insert(self.ticket);
			return;
		}
		let mut serving = state.serving + 1;
		while state.abandoned.remove(&serving) {
			serving += 1;
		}
		state.serving = serving;
		if let Some(waker) = state.waiting.remove(&serving) {
			waker.wake();
		}
	}
}

#[cfg(test)]
mod tests {
	use super::{RusqliteOptions, SqliteMode, open_rusqlite_database};
	use crate::sqlite::{SqliteDatabase, SqliteError, SqliteExecutor, SqliteRow, SqliteValue};
	use crate::test_executor::{Executor, now};
	use std::path::PathBuf;
	use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
	use std::sync::{Arc, Mutex};
	use tokio_util::sync::CancellationToken;

	/// A callback's own error, or the driver's.
	#[derive(Debug, thiserror::Error)]
	enum Failure {
		#[error("{0}")]
		Own(&'static str),
		#[error(transparent)]
		Sqlite(#[from] SqliteError),
	}

	fn memory() -> super::RusqliteDatabase {
		now(open_rusqlite_database(":memory:", RusqliteOptions::default()))
			.unwrap()
			.unwrap()
	}

	fn ready<T>(future: impl std::future::Future<Output = T>) -> T {
		now(future).unwrap()
	}

	fn integer(row: Option<SqliteRow>, column: &str) -> i64 {
		match row.and_then(|row| row.get(column).cloned()) {
			Some(SqliteValue::Integer(value)) => value,
			other => panic!("{column}: {other:?}"),
		}
	}

	fn integers(rows: Vec<SqliteRow>, column: &str) -> Vec<i64> {
		rows.into_iter().map(|row| integer(Some(row), column)).collect()
	}

	fn text(value: &str) -> SqliteValue {
		SqliteValue::Text(value.to_owned())
	}

	/// A fresh directory under the system temp directory.
	fn temp_dir() -> PathBuf {
		static NEXT: AtomicUsize = AtomicUsize::new(0);
		let name = format!(
			"pi-chord-sqlite-{}-{}",
			std::process::id(),
			NEXT.fetch_add(1, Ordering::SeqCst)
		);
		let dir = std::env::temp_dir().join(name);
		std::fs::create_dir_all(&dir).unwrap();
		dir
	}

	// TS: sqlite/node.test.ts > Node SQLite driver > configures connection defaults and accepts explicit settings
	#[test]
	fn configures_connection_defaults_and_accepts_explicit_settings() {
		for (options, timeout, pages) in [
			(RusqliteOptions::default(), 5_000, 1_000),
			(
				RusqliteOptions {
					busy_timeout_ms: 123,
					wal_auto_checkpoint_pages: 0,
					..RusqliteOptions::default()
				},
				123,
				0,
			),
		] {
			let database = ready(open_rusqlite_database(":memory:", options)).unwrap();
			assert_eq!(
				integer(ready(database.get("PRAGMA busy_timeout", &[])).unwrap(), "timeout"),
				timeout
			);
			assert_eq!(
				integer(
					ready(database.get("PRAGMA wal_autocheckpoint", &[])).unwrap(),
					"wal_autocheckpoint"
				),
				pages
			);
			assert_eq!(
				integer(ready(database.get("PRAGMA synchronous", &[])).unwrap(), "synchronous"),
				1
			);
			ready(database.close()).unwrap();
		}
	}

	// TS: sqlite/node.test.ts > Node SQLite driver > keeps the most recently used prepared statements and prepares
	// evicted ones again (the results; rusqlite's statement cache is the same 256-entry LRU, its prepares are not
	// observable from outside)
	#[test]
	fn reuses_cached_statements_across_many_shapes() {
		let database = memory();
		for index in 0..300 {
			assert_eq!(
				integer(ready(database.get(&format!("SELECT {index} AS n"), &[])).unwrap(), "n"),
				index
			);
		}
		assert_eq!(integer(ready(database.get("SELECT 299 AS n", &[])).unwrap(), "n"), 299);
		assert_eq!(integer(ready(database.get("SELECT 0 AS n", &[])).unwrap(), "n"), 0);
	}

	// TS: sqlite/node.test.ts > Node SQLite driver > reports the rows a statement changed, without index writes
	#[test]
	fn reports_the_rows_a_statement_changed_without_index_writes() {
		let database = memory();
		ready(database.exec(
			"CREATE TABLE changes_probe (id TEXT PRIMARY KEY, path TEXT NOT NULL, n INTEGER NOT NULL) STRICT, WITHOUT ROWID",
		))
		.unwrap();
		ready(database.exec("CREATE INDEX changes_probe_path ON changes_probe (path, id)")).unwrap();
		ready(database.exec("CREATE INDEX changes_probe_n ON changes_probe (n, id)")).unwrap();
		for index in 0..10 {
			let params = [
				text(&format!("k{index}")),
				text(&format!("a/{index}")),
				SqliteValue::Integer(index),
			];
			ready(database.run("INSERT INTO changes_probe VALUES (?, ?, ?)", &params)).unwrap();
		}
		let counted = |sql: &str, params: &[SqliteValue]| ready(database.run_counted(sql, params)).unwrap();
		let changes = [
			counted(
				"INSERT INTO changes_probe VALUES (?, ?, ?)",
				&[text("x"), text("a/x"), SqliteValue::Integer(99)],
			),
			counted(
				"UPDATE changes_probe SET path = ? WHERE id = ?",
				&[text("b/x"), text("x")],
			),
			counted("UPDATE changes_probe SET n = 0 WHERE id = ?", &[text("absent")]),
			counted(
				"UPDATE changes_probe SET path = 'c/' || substr(path, 3) WHERE path >= 'a/' AND path < 'a0'",
				&[],
			),
			counted(
				"INSERT INTO changes_probe VALUES (?, ?, ?) ON CONFLICT (id) DO UPDATE SET n = excluded.n",
				&[text("x"), text("b/x"), SqliteValue::Integer(5)],
			),
			counted("DELETE FROM changes_probe WHERE id = ?", &[text("x")]),
			counted("DELETE FROM changes_probe WHERE path >= 'c/' AND path < 'c0'", &[]),
		];
		assert_eq!(changes, [1, 1, 0, 10, 1, 1, 10]);
		let in_transaction = ready(database.transaction(|handle| async move {
			let params = [text("t"), text("t/1"), SqliteValue::Integer(1)];
			handle
				.run_counted("INSERT INTO changes_probe VALUES (?, ?, ?)", &params)
				.await
				.map_err(Failure::from)
		}));
		assert_eq!(in_transaction.unwrap(), 1);
	}

	// TS: sqlite/node.test.ts > Node SQLite driver > commits work done through the transaction handle and closes
	// idempotently
	#[test]
	fn commits_work_done_through_the_transaction_handle_and_closes_idempotently() {
		let database = memory();
		let committed: Result<(), Failure> = ready(database.transaction(|transaction| async move {
			transaction.exec("CREATE TABLE async_probe (value INTEGER)").await?;
			transaction
				.run("INSERT INTO async_probe (value) VALUES (?)", &[SqliteValue::Integer(1)])
				.await?;
			Ok(())
		}));
		committed.unwrap();
		assert_eq!(
			integer(
				ready(database.get("SELECT value FROM async_probe", &[])).unwrap(),
				"value"
			),
			1
		);
		ready(database.close()).unwrap();
		ready(database.close()).unwrap();
		assert_eq!(ready(database.exec("SELECT 1")), Err(SqliteError::Closed));
	}

	// TS: sqlite/node.test.ts > Node SQLite driver > serializes concurrent transactions, and queues ordinary operations
	// behind an active transaction
	#[test]
	fn serializes_transactions_and_queues_operations_behind_one() {
		let executor = Executor::default();
		// Leaked: the executor's futures borrow the handle for the rest of the test process.
		let database: &'static super::RusqliteDatabase = Box::leak(Box::new(memory()));
		ready(database.exec("CREATE TABLE queue (value INTEGER)")).unwrap();
		let (gate, started) = (CancellationToken::new(), Arc::new(AtomicBool::new(false)));
		let (first_gate, first_started) = (gate.clone(), started.clone());
		let first = database.transaction(move |transaction| async move {
			transaction.exec("INSERT INTO queue (value) VALUES (1)").await?;
			first_started.store(true, Ordering::SeqCst);
			first_gate.cancelled_owned().await;
			Ok::<(), Failure>(())
		});
		executor.spawn(async move { first.await.unwrap() });
		executor.run();
		assert!(started.load(Ordering::SeqCst));
		let second_started = Arc::new(AtomicBool::new(false));
		let flag = second_started.clone();
		let second = database.transaction(move |transaction| async move {
			flag.store(true, Ordering::SeqCst);
			transaction.exec("INSERT INTO queue (value) VALUES (2)").await?;
			Ok::<(), Failure>(())
		});
		executor.spawn(async move { second.await.unwrap() });
		let write = database.exec("INSERT INTO queue (value) VALUES (3)");
		let read_rows: Arc<Mutex<Option<Vec<i64>>>> = Arc::default();
		let (rows, read) = (
			read_rows.clone(),
			database.all("SELECT value FROM queue ORDER BY value", &[]),
		);
		executor.spawn(async move { write.await.unwrap() });
		executor.spawn(async move {
			let values = integers(read.await.unwrap(), "value");
			*rows.lock().unwrap() = Some(values);
		});
		executor.run();
		assert!(!second_started.load(Ordering::SeqCst));
		assert!(read_rows.lock().unwrap().is_none());
		gate.cancel();
		assert_eq!(executor.run(), 0);
		assert_eq!(*read_rows.lock().unwrap(), Some(vec![1, 2, 3]));
	}

	/// A database call taken inside a transaction callback, awaited after it.
	type Queued = std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SqliteError>> + Send>>;

	// TS: sqlite/node.test.ts > Node SQLite driver > runs operations in call order whether they start immediately or
	// wait, and queues database calls made synchronously by a transaction that started immediately
	#[test]
	fn runs_operations_in_call_order() {
		let executor = Executor::default();
		// Leaked: the executor's futures borrow the handle for the rest of the test process.
		let database: &'static super::RusqliteDatabase = Box::leak(Box::new(memory()));
		ready(database.exec("CREATE TABLE call_order (value INTEGER)")).unwrap();
		let outside: Arc<Mutex<Option<Queued>>> = Arc::default();
		let slot = outside.clone();
		let transaction = database.transaction(move |handle| async move {
			// Misuse: a call on the database from inside takes a turn after the transaction instead of joining it.
			let params = Box::leak(Box::new([SqliteValue::Integer(2)]));
			*slot.lock().unwrap() = Some(Box::pin(
				database.run("INSERT INTO call_order (value) VALUES (?)", params),
			));
			handle
				.run("INSERT INTO call_order (value) VALUES (?)", &[SqliteValue::Integer(1)])
				.await?;
			Err::<(), Failure>(Failure::Own("roll back"))
		});
		let before = database.all("SELECT value FROM call_order ORDER BY value", &[]);
		let write = database.run(
			"INSERT INTO call_order (value) VALUES (?)",
			Box::leak(Box::new([SqliteValue::Integer(3)])),
		);
		let after = database.all("SELECT value FROM call_order ORDER BY value", &[]);
		let results: Arc<Mutex<Vec<Vec<i64>>>> = Arc::default();
		let (on_before, on_after) = (results.clone(), results.clone());
		executor.spawn(async move { assert_eq!(transaction.await.unwrap_err().to_string(), "roll back") });
		executor.spawn(async move {
			let values = integers(before.await.unwrap(), "value");
			on_before.lock().unwrap().push(values);
		});
		executor.spawn(async move { write.await.unwrap() });
		executor.spawn(async move {
			let values = integers(after.await.unwrap(), "value");
			on_after.lock().unwrap().push(values);
		});
		assert_eq!(executor.run(), 0);
		assert_eq!(*results.lock().unwrap(), [vec![], vec![3]]);
		let late = outside.lock().unwrap().take().unwrap();
		ready(late).unwrap();
		assert_eq!(
			integers(
				ready(database.all("SELECT value FROM call_order ORDER BY value", &[])).unwrap(),
				"value"
			),
			[2, 3]
		);
	}

	// TS: sqlite/node.test.ts > Node SQLite driver > rejects a transaction handle used after its transaction settles
	#[test]
	fn rejects_a_transaction_handle_used_after_its_transaction_settles() {
		let database = memory();
		ready(database.exec("CREATE TABLE stale_probe (value INTEGER)")).unwrap();
		let kept: Arc<Mutex<Option<super::RusqliteTransaction>>> = Arc::default();
		let slot = kept.clone();
		let done: Result<(), Failure> = ready(database.transaction(move |transaction| async move {
			transaction
				.run("INSERT INTO stale_probe (value) VALUES (?)", &[SqliteValue::Integer(1)])
				.await?;
			*slot.lock().unwrap() = Some(transaction);
			Ok(())
		}));
		done.unwrap();
		let handle = kept.lock().unwrap().take().unwrap();
		assert_eq!(
			ready(handle.exec("INSERT INTO stale_probe (value) VALUES (2)")),
			Err(SqliteError::InactiveTransaction)
		);
		assert_eq!(
			SqliteError::InactiveTransaction.to_string(),
			"SQLite transaction handle is no longer active"
		);
		assert_eq!(
			integers(
				ready(database.all("SELECT value FROM stale_probe", &[])).unwrap(),
				"value"
			),
			[1]
		);
	}

	// TS: sqlite/node.test.ts > Node SQLite driver > does not preserve the callback error when rollback itself fails
	// (and the outcome of a transaction > rejects with an AggregateError when its ROLLBACK fails)
	#[test]
	fn does_not_preserve_the_callback_error_when_rollback_itself_fails() {
		let database = memory();
		ready(database.exec("CREATE TABLE rollback_probe (value INTEGER)")).unwrap();
		let failed: Result<(), Failure> = ready(database.transaction(|transaction| async move {
			transaction
				.exec("INSERT INTO rollback_probe (value) VALUES (1)")
				.await?;
			transaction.exec("COMMIT").await?;
			Err(Failure::Own("rejected after an escaped commit"))
		}));
		assert!(
			matches!(failed, Err(Failure::Sqlite(SqliteError::RollbackFailed { .. }))),
			"{failed:?}"
		);
		assert_eq!(
			integer(
				ready(database.get("SELECT value FROM rollback_probe", &[])).unwrap(),
				"value"
			),
			1
		);
	}

	// TS: sqlite/node.test.ts > the outcome of a transaction > rejects with the callback's own error after a rollback
	#[test]
	fn rejects_with_the_callbacks_own_error_after_a_rollback() {
		let database = memory();
		ready(database.exec("CREATE TABLE t (n INTEGER)")).unwrap();
		let failed: Result<(), Failure> = ready(database.transaction(|handle| async move {
			handle.run("INSERT INTO t VALUES (1)", &[]).await?;
			Err(Failure::Own("refused"))
		}));
		assert!(matches!(failed, Err(Failure::Own("refused"))));
		assert_eq!(ready(database.all("SELECT n FROM t", &[])).unwrap().len(), 0);
	}

	// TS: sqlite/node.test.ts > the outcome of a transaction > rejects with another error when %s fails (COMMIT through
	// a deferred foreign key, BEGIN IMMEDIATE through a competing writer)
	#[test]
	fn rejects_with_another_error_when_begin_or_commit_fails() {
		let database = memory();
		let schema = concat!(
			"CREATE TABLE parent (id INTEGER PRIMARY KEY); ",
			"CREATE TABLE child (parent INTEGER REFERENCES parent (id) DEFERRABLE INITIALLY DEFERRED)",
		);
		ready(database.exec(schema)).unwrap();
		let commit: Result<i32, Failure> = ready(database.transaction(|handle| async move {
			handle.run("INSERT INTO child VALUES (7)", &[]).await?;
			Ok(1)
		}));
		assert_eq!(commit.unwrap_err().to_string(), "FOREIGN KEY constraint failed");
		assert_eq!(ready(database.all("SELECT * FROM child", &[])).unwrap().len(), 0);

		let dir = temp_dir();
		let file = dir.join("locked.sqlite");
		let writer = ready(open_rusqlite_database(&file, RusqliteOptions::default())).unwrap();
		let other = ready(open_rusqlite_database(
			&file,
			RusqliteOptions {
				busy_timeout_ms: 0,
				..RusqliteOptions::default()
			},
		))
		.unwrap();
		ready(writer.exec("CREATE TABLE t (n INTEGER); BEGIN IMMEDIATE")).unwrap();
		let begin: Result<i32, Failure> = ready(other.transaction(|_| async { Ok(1) }));
		assert_eq!(begin.unwrap_err().to_string(), "database is locked");
		ready(writer.exec("COMMIT")).unwrap();
		ready(other.close()).unwrap();
		ready(writer.close()).unwrap();
		std::fs::remove_dir_all(dir).unwrap();
	}

	// TS: sqlite/node.test.ts > read-only connections > read a file another connection writes, in deferred
	// transactions, and never write it
	#[test]
	fn read_only_connections_read_one_snapshot_per_transaction_and_never_write() {
		let dir = temp_dir();
		let file = dir.join("nested").join("data.sqlite");
		let writer = ready(open_rusqlite_database(&file, RusqliteOptions::default())).unwrap();
		ready(writer.exec("CREATE TABLE probe (value INTEGER)")).unwrap();
		ready(writer.run("INSERT INTO probe VALUES (1)", &[])).unwrap();
		let read_only = RusqliteOptions {
			mode: SqliteMode::ReadOnly,
			..RusqliteOptions::default()
		};
		let reader = ready(open_rusqlite_database(&file, read_only)).unwrap();
		let inner_writer = writer.clone();
		let snapshots: Result<(Vec<i64>, Vec<i64>), Failure> = ready(reader.transaction(move |tx| async move {
			let before = integers(tx.all("SELECT value FROM probe", &[]).await?, "value");
			inner_writer.run("INSERT INTO probe VALUES (2)", &[]).await?;
			Ok((before, integers(tx.all("SELECT value FROM probe", &[]).await?, "value")))
		}));
		assert_eq!(snapshots.unwrap(), (vec![1], vec![1]));
		assert_eq!(
			integers(ready(reader.all("SELECT value FROM probe", &[])).unwrap(), "value"),
			[1, 2]
		);
		let refused = ready(reader.run("INSERT INTO probe VALUES (3)", &[])).unwrap_err();
		assert!(refused.to_string().contains("readonly"), "{refused}");
		ready(reader.close()).unwrap();
		ready(writer.close()).unwrap();
		let missing = RusqliteOptions {
			mode: SqliteMode::ReadOnly,
			..RusqliteOptions::default()
		};
		assert!(ready(open_rusqlite_database(dir.join("missing.sqlite"), missing)).is_err());
		std::fs::remove_dir_all(dir).unwrap();
	}

	// Rust-only (D0b review): a transaction future dropped mid-callback rolls back and passes its turn on.
	#[test]
	fn a_dropped_transaction_rolls_back_and_passes_its_turn_on() {
		let dir = temp_dir();
		let file = dir.join("dropped.sqlite");
		let database = ready(open_rusqlite_database(&file, RusqliteOptions::default())).unwrap();
		ready(database.exec("CREATE TABLE t (n INTEGER)")).unwrap();
		let gate = CancellationToken::new();
		let wait = gate.clone();
		let mut pending = Box::pin(database.transaction(move |handle| async move {
			handle.run("INSERT INTO t VALUES (1)", &[]).await?;
			wait.cancelled_owned().await;
			Ok::<(), Failure>(())
		}));
		assert!(now(pending.as_mut()).is_none());
		drop(pending);
		ready(database.run("INSERT INTO t VALUES (2)", &[])).unwrap();
		let second: Result<(), Failure> = ready(database.transaction(|handle| async move {
			handle.run("INSERT INTO t VALUES (3)", &[]).await?;
			Ok(())
		}));
		second.unwrap();
		ready(database.close()).unwrap();
		let reopened = ready(open_rusqlite_database(&file, RusqliteOptions::default())).unwrap();
		let rows = ready(reopened.all("SELECT n FROM t ORDER BY n", &[])).unwrap();
		assert_eq!(integers(rows, "n"), [2, 3]);
		ready(reopened.close()).unwrap();
		std::fs::remove_dir_all(dir).unwrap();
	}

	// Rust-only (D0b review): `node:sqlite`'s defaults: double-quoted string literals are refused, in queries and in
	// schema statements, on read-write and read-only connections alike.
	#[test]
	fn refuses_double_quoted_string_literals() {
		let database = memory();
		let query = ready(database.get(r#"SELECT "text" AS value"#, &[])).unwrap_err();
		assert!(query.to_string().contains("no such column"), "{query}");
		let schema = ready(database.exec(r#"CREATE TABLE t (n TEXT DEFAULT "x", CHECK (n != "y"))"#));
		assert!(schema.is_err(), "{schema:?}");
		let dir = temp_dir();
		let file = dir.join("dqs.sqlite");
		let writer = ready(open_rusqlite_database(&file, RusqliteOptions::default())).unwrap();
		ready(writer.exec("CREATE TABLE t (n INTEGER)")).unwrap();
		let read_only = RusqliteOptions {
			mode: SqliteMode::ReadOnly,
			..RusqliteOptions::default()
		};
		let reader = ready(open_rusqlite_database(&file, read_only)).unwrap();
		assert!(ready(reader.get(r#"SELECT "text" AS value"#, &[])).is_err());
		ready(reader.close()).unwrap();
		ready(writer.close()).unwrap();
		std::fs::remove_dir_all(dir).unwrap();
	}

	// Rust-only (D0b review): foreign keys are enforced without the caller turning them on, as `node:sqlite` does.
	#[test]
	fn enforces_foreign_keys() {
		let database = memory();
		assert_eq!(
			integer(ready(database.get("PRAGMA foreign_keys", &[])).unwrap(), "foreign_keys"),
			1
		);
		ready(database.exec(
			"CREATE TABLE parent (id INTEGER PRIMARY KEY); CREATE TABLE child (parent INTEGER REFERENCES parent (id))",
		))
		.unwrap();
		let refused = ready(database.run("INSERT INTO child VALUES (7)", &[])).unwrap_err();
		assert_eq!(refused.to_string(), "FOREIGN KEY constraint failed");
	}
}
