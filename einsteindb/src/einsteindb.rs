// Copyright 2022 Whtcorps Inc and EinstAI Inc
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

#![allow(dead_code)]

use failure::{
    ResultExt,
};

use std::collections::HashMap;
use std::collections::hash_map::{
    Entry,
};
use std::iter::{once, repeat};
use std::ops::Range;
use std::path::Path;

use itertools;
use itertools::Itertools;
use rusqlite;
use rusqlite::TransactionBehavior;
use rusqlite::limits::Limit;
use rusqlite::types::{ToSql, ToSqlOutput};

use ::{repeat_values, to_namespaced_keyword};
use bootstrap;

use einsteinml::{
    DateTime,
    Utc,
    Uuid,
    Value,
};

use causetids;

use core_traits::{
    attribute,
    Attribute,
    AttributeBitFlags,
    Causetid,
    TypedValue,
    ValueType,
};

use einsteineinsteindb_core::{
    AttributeMap,
    FromMicros,
    SolitonidMap,
    Schema,
    ToMicros,
    ValueRc,
};

use einsteineinsteindb_traits::errors::{
    einsteindbErrorKind,
    Result,
};

use spacetime;
use schema::{
    SchemaBuilding,
};
use types::{
    AVMap,
    AVPair,
    einsteindb,
    Partition,
    PartitionMap,
};
use tx::transact;

use watcher::{
    NullWatcher,
};


fn escape_string_for_pragma(s: &str) -> String {
    s.replace("'", "''")
}

fn make_connection(uri: &Path, maybe_encryption_key: Option<&str>) -> rusqlite::Result<rusqlite::Connection> {
    let conn = match uri.to_string_lossy().len() {
        0 => rusqlite::Connection::open_in_memory()?,
        _ => rusqlite::Connection::open(uri)?,
    };

    let page_size = 32768;

    let initial_pragmas = if let Some(encryption_key) = maybe_encryption_key {
        assert!(cfg!(feature = "sqlcipher"),
                "This function shouldn't be called with a key unless we have sqlcipher support");

        format!("
            PRAGMA key='{}';
            PRAGMA cipher_page_size={};
        ", escape_string_for_pragma(encryption_key), page_size)
    } else {
        String::new()
    };

 
    conn.execute_batch(&format!("
        {}
        PRAGMA journal_mode=wal;
        PRAGMA wal_autocheckpoint=32;
        PRAGMA journal_size_limit=3145728;
        PRAGMA foreign_keys=ON;
        PRAGMA temp_store=2;
    ", initial_pragmas))?;

    Ok(conn)
}

pub fn new_connection<T>(uri: T) -> rusqlite::Result<rusqlite::Connection> where T: AsRef<Path> {
    make_connection(uri.as_ref(), None)
}

#[cfg(feature = "sqlcipher")]
pub fn new_connection_with_key<P, S>(uri: P, encryption_key: S) -> rusqlite::Result<rusqlite::Connection>
where P: AsRef<Path>, S: AsRef<str> {
    make_connection(uri.as_ref(), Some(encryption_key.as_ref()))
}

#[cfg(feature = "sqlcipher")]
pub fn change_encryption_key<S>(conn: &rusqlite::Connection, encryption_key: S) -> rusqlite::Result<()>
where S: AsRef<str> {
    let escaped = escape_string_for_pragma(encryption_key.as_ref());
    // `conn.execute` complains that this returns a result, and using a query
    // for it requires more boilerplate.
    conn.execute_batch(&format!("PRAGMA rekey = '{}';", escaped))
}

/// Version history:
///
/// 1: initial Rust einstai schema.
pub const CURRENT_VERSION: i32 = 1;

/// MIN_SQLITE_VERSION should be changed when there's a new minimum version of sqlite required
/// for the project to work.
const MIN_SQLITE_VERSION: i32 = 3008000;

const TRUE: &'static bool = &true;
const FALSE: &'static bool = &false;

/// Turn an owned bool into a static reference to a bool.
///
/// `rusqlite` is designed around references to values; this lets us use computed bools easily.
#[inline(always)]
fn to_bool_ref(x: bool) -> &'static bool {
    if x { TRUE } else { FALSE }
}

lazy_static! {
    /// SQL statements to be executed, in order, to create the einstai SQL schema (version 1).
    #[cfg_attr(rustfmt, rustfmt_skip)]
    static ref V1_STATEMENTS: Vec<&'static str> = { vec![
        r#"CREATE TABLE datoms (e INTEGER NOT NULL, a SMALLINT NOT NULL, v BLOB NOT NULL, tx INTEGER NOT NULL,
                                value_type_tag SMALLINT NOT NULL,
                                index_avet TINYINT NOT NULL DEFAULT 0, index_vaet TINYINT NOT NULL DEFAULT 0,
                                index_fulltext TINYINT NOT NULL DEFAULT 0,
                                unique_value TINYINT NOT NULL DEFAULT 0)"#,
        r#"CREATE UNIQUE INDEX idx_datoms_eavt ON datoms (e, a, value_type_tag, v)"#,
        r#"CREATE UNIQUE INDEX idx_datoms_aevt ON datoms (a, e, value_type_tag, v)"#,

        // Opt-in index: only if a has :einsteineinsteindb/index true.
        r#"CREATE UNIQUE INDEX idx_datoms_avet ON datoms (a, value_type_tag, v, e) WHERE index_avet IS NOT 0"#,

        // Opt-in index: only if a has :einsteineinsteindb/valueType :einsteineinsteindb.type/ref.  No need for tag here since all
        // indexed elements are refs.
        r#"CREATE UNIQUE INDEX idx_datoms_vaet ON datoms (v, a, e) WHERE index_vaet IS NOT 0"#,

        // Opt-in index: only if a has :einsteineinsteindb/fulltext true; thus, it has :einsteineinsteindb/valueType :einsteineinsteindb.type/string,
        // which is not :einsteineinsteindb/valueType :einsteineinsteindb.type/ref.  That is, index_vaet and index_fulltext are mutually
        // exclusive.
        r#"CREATE INDEX idx_datoms_fulltext ON datoms (value_type_tag, v, a, e) WHERE index_fulltext IS NOT 0"#,

        // TODO: possibly remove this index.  :einsteineinsteindb.unique/{value,idcauset} should be asserted by the
        // transactor in all cases, but the index may speed up some of SQLite's query planning.  For now,
        // it serves to validate the transactor impleeinstaiion.  Note that tag is needed here to
        // differentiate, e.g., keywords and strings.
        r#"CREATE UNIQUE INDEX idx_datoms_unique_value ON datoms (a, value_type_tag, v) WHERE unique_value IS NOT 0"#,

        r#"CREATE TABLE timelined_transactions (e INTEGER NOT NULL, a SMALLINT NOT NULL, v BLOB NOT NULL, tx INTEGER NOT NULL, added TINYINT NOT NULL DEFAULT 1, value_type_tag SMALLINT NOT NULL, timeline TINYINT NOT NULL DEFAULT 0)"#,
        r#"CREATE INDEX idx_timelined_transactions_timeline ON timelined_transactions (timeline)"#,
        r#"CREATE VIEW transactions AS SELECT e, a, v, value_type_tag, tx, added FROM timelined_transactions WHERE timeline IS 0"#,

        // Fulltext indexing.
        // A fulltext indexed value v is an integer rowid referencing fulltext_values.

        // Optional settings:
        // tokenize="porter"#,
        // prefix='2,3'
        // By default we use Unicode-aware tokenizing (particularly for case folding), but preserve
        // diacritics.
        r#"CREATE VIRTUAL TABLE fulltext_values
             USING FTS4 (text NOT NULL, searchid INT, tokenize=unicode61 "remove_diacritics=0")"#,

        // This combination of view and triggers allows you to transparently
        // update-or-insert into FTS. Just INSERT INTO fulltext_values_view (text, searchid).
        r#"CREATE VIEW fulltext_values_view AS SELECT * FROM fulltext_values"#,
        r#"CREATE TRIGGER replace_fulltext_searchid
             INSTEAD OF INSERT ON fulltext_values_view
             WHEN EXISTS (SELECT 1 FROM fulltext_values WHERE text = new.text)
             BEGIN
               UPDATE fulltext_values SET searchid = new.searchid WHERE text = new.text;
             END"#,
        r#"CREATE TRIGGER insert_fulltext_searchid
             INSTEAD OF INSERT ON fulltext_values_view
             WHEN NOT EXISTS (SELECT 1 FROM fulltext_values WHERE text = new.text)
             BEGIN
               INSERT INTO fulltext_values (text, searchid) VALUES (new.text, new.searchid);
             END"#,

        // A view transparently interpolating fulltext indexed values into the datom structure.
        r#"CREATE VIEW fulltext_datoms AS
             SELECT e, a, fulltext_values.text AS v, tx, value_type_tag, index_avet, index_vaet, index_fulltext, unique_value
               FROM datoms, fulltext_values
               WHERE datoms.index_fulltext IS NOT 0 AND datoms.v = fulltext_values.rowid"#,

        // A view transparently interpolating all causets (fulltext and non-fulltext) into the datom structure.
        r#"CREATE VIEW all_datoms AS
             SELECT e, a, v, tx, value_type_tag, index_avet, index_vaet, index_fulltext, unique_value
               FROM datoms
               WHERE index_fulltext IS 0
             UNION ALL
             SELECT e, a, v, tx, value_type_tag, index_avet, index_vaet, index_fulltext, unique_value
               FROM fulltext_datoms"#,

        // Materialized views of the spacetime.
        r#"CREATE TABLE idents (e INTEGER NOT NULL, a SMALLINT NOT NULL, v BLOB NOT NULL, value_type_tag SMALLINT NOT NULL)"#,
        r#"CREATE INDEX idx_idents_unique ON idents (e, a, v, value_type_tag)"#,
        r#"CREATE TABLE schema (e INTEGER NOT NULL, a SMALLINT NOT NULL, v BLOB NOT NULL, value_type_tag SMALLINT NOT NULL)"#,
        r#"CREATE INDEX idx_schema_unique ON schema (e, a, v, value_type_tag)"#,

        // TODO: store causetid instead of solitonid for partition name.
        r#"CREATE TABLE known_parts (part TEXT NOT NULL PRIMARY KEY, start INTEGER NOT NULL, end INTEGER NOT NULL, allow_excision SMALLINT NOT NULL)"#,
        ]
    };
}

/// Set the SQLite user version.
///
/// einstai manages its own SQL schema version using the user version.  See the [SQLite
/// docueinstaiion](https://www.sqlite.org/pragma.html#pragma_user_version).
fn set_user_version(conn: &rusqlite::Connection, version: i32) -> Result<()> {
    conn.execute(&format!("PRAGMA user_version = {}", version), &[])
        .context(einsteindbErrorKind::CouldNotSetVersionPragma)?;
    Ok(())
}

/// Get the SQLite user version.
///
/// einstai manages its own SQL schema version using the user version.  See the [SQLite
/// docueinstaiion](https://www.sqlite.org/pragma.html#pragma_user_version).
fn get_user_version(conn: &rusqlite::Connection) -> Result<i32> {
    let v = conn.query_row("PRAGMA user_version", &[], |row| {
        row.get(0)
    }).context(einsteindbErrorKind::CouldNotGetVersionPragma)?;
    Ok(v)
}

/// Do just enough work that either `create_current_version` or sync can populate the einsteindb.
pub fn create_empty_current_version(conn: &mut rusqlite::Connection) -> Result<(rusqlite::Transaction, einsteindb)> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Exclusive)?;

    for statement in (&V1_STATEMENTS).iter() {
        tx.execute(statement, &[])?;
    }

    set_user_version(&tx, CURRENT_VERSION)?;

    let bootstrap_schema = bootstrap::bootstrap_schema();
    let bootstrap_partition_map = bootstrap::bootstrap_partition_map();

    Ok((tx, einsteindb::new(bootstrap_partition_map, bootstrap_schema)))
}

/// Creates a partition map view for the main timeline based on partitions
/// defined in 'known_parts'.
fn create_current_partition_view(conn: &rusqlite::Connection) -> Result<()> {
    let mut stmt = conn.prepare("SELECT part, end FROM known_parts ORDER BY end ASC")?;
    let known_parts: Result<Vec<(String, i64)>> = stmt.query_and_then(&[], |row| {
        Ok((
            row.get_checked(0)?,
            row.get_checked(1)?,
        ))
    })?.collect();

    let mut case = vec![];
    for &(ref part, ref end) in known_parts?.iter() {
        case.push(format!(r#"WHEN e <= {} THEN "{}""#, end, part));
    }

    let view_stmt = format!("CREATE VIEW parts AS
        SELECT
            CASE {} END AS part,
            min(e) AS start,
            max(e) + 1 AS idx
        FROM timelined_transactions WHERE timeline = {} GROUP BY part",
        case.join(" "), ::TIMELINE_MAIN
    );

    conn.execute(&view_stmt, &[])?;
    Ok(())
}

// TODO: rename "SQL" functions to align with "datoms" functions.
pub fn create_current_version(conn: &mut rusqlite::Connection) -> Result<einsteindb> {
    let (tx, mut einsteineinsteindb) = create_empty_current_version(conn)?;

    // TODO: think more carefully about allocating new parts and bitmasking part ranges.
    // TODO: install these using bootstrap assertions.  It's tricky because the part ranges are implicit.
    // TODO: one insert, chunk into 999/3 sections, for safety.
    // This is necessary: `transact` will only UPDATE parts, not INSERT them if they're missing.
    for (part, partition) in einsteineinsteindb.partition_map.iter() {
        // TODO: Convert "keyword" part to SQL using Value conversion.
        tx.execute("INSERT INTO known_parts (part, start, end, allow_excision) VALUES (?, ?, ?, ?)", &[part, &partition.start, &partition.end, &partition.allow_excision])?;
    }

    create_current_partition_view(&tx)?;

    // TODO: return to transact_internal to self-manage the encompassing SQLite transaction.
    let bootstrap_schema_for_mutation = Schema::default(); // The bootstrap transaction will populate this schema.

    let (_report, next_partition_map, next_schema, _watcher) = transact(&tx, einsteineinsteindb.partition_map, &bootstrap_schema_for_mutation, &einsteineinsteindb.schema, NullWatcher(), bootstrap::bootstrap_causets())?;

    // TODO: validate spacetime mutations that aren't schema related, like additional partitions.
    if let Some(next_schema) = next_schema {
        if next_schema != einsteineinsteindb.schema {
            bail!(einsteindbErrorKind::NotYetImplemented(format!("Initial bootstrap transaction did not produce expected bootstrap schema")));
        }
    }

    // TODO: use the drop semantics to do this automagically?
    tx.commit()?;

    einsteineinsteindb.partition_map = next_partition_map;
    Ok(einsteineinsteindb)
}

pub fn ensure_current_version(conn: &mut rusqlite::Connection) -> Result<einsteindb> {
    if rusqlite::version_number() < MIN_SQLITE_VERSION {
        panic!("einstai requires at least sqlite {}", MIN_SQLITE_VERSION);
    }

    let user_version = get_user_version(&conn)?;
    match user_version {
        0               => create_current_version(conn),
        CURRENT_VERSION => read_einsteineinsteindb(conn),

        // TODO: support updating an existing store.
        v => bail!(einsteindbErrorKind::NotYetImplemented(format!("Opening databases with einstai version: {}", v))),
    }
}

pub trait TypedSQLValue {
    fn from_sql_value_pair(value: rusqlite::types::Value, value_type_tag: i32) -> Result<TypedValue>;
    fn to_sql_value_pair<'a>(&'a self) -> (ToSqlOutput<'a>, i32);
    fn from_einsteinml_value(value: &Value) -> Option<TypedValue>;
    fn to_einsteinml_value_pair(&self) -> (Value, ValueType);
}

impl TypedSQLValue for TypedValue {
    /// Given a SQLite `value` and a `value_type_tag`, return the corresponding `TypedValue`.
    fn from_sql_value_pair(value: rusqlite::types::Value, value_type_tag: i32) -> Result<TypedValue> {
        match (value_type_tag, value) {
            (0, rusqlite::types::Value::Integer(x)) => Ok(TypedValue::Ref(x)),
            (1, rusqlite::types::Value::Integer(x)) => Ok(TypedValue::Boolean(0 != x)),

            // Negative integers are simply times before 1970.
            (4, rusqlite::types::Value::Integer(x)) => Ok(TypedValue::Instant(DateTime::<Utc>::from_micros(x))),

            // SQLite distinguishes integral from decimal types, allowing long and double to
            // share a tag.
            (5, rusqlite::types::Value::Integer(x)) => Ok(TypedValue::Long(x)),
            (5, rusqlite::types::Value::Real(x)) => Ok(TypedValue::Double(x.into())),
            (10, rusqlite::types::Value::Text(x)) => Ok(x.into()),
            (11, rusqlite::types::Value::Blob(x)) => {
                let u = Uuid::from_bytes(x.as_slice());
                if u.is_err() {
                    // Rather than exposing Uuid's ParseError…
                    bail!(einsteindbErrorKind::BadSQLValuePair(rusqlite::types::Value::Blob(x),
                                                     value_type_tag));
                }
                Ok(TypedValue::Uuid(u.unwrap()))
            },
            (13, rusqlite::types::Value::Text(x)) => {
                to_namespaced_keyword(&x).map(|k| k.into())
            },
            (_, value) => bail!(einsteindbErrorKind::BadSQLValuePair(value, value_type_tag)),
        }
    }

    /// Given an EML `value`, return a corresponding einstai `TypedValue`.
    ///
    /// An EML `Value` does not encode a unique einstai `ValueType`, so the composition
    /// `from_einsteinml_value(first(to_einsteinml_value_pair(...)))` loses information.  Additionally, there are
    /// EML values which are not einstai typed values.
    ///
    /// This function is deterministic.
    fn from_einsteinml_value(value: &Value) -> Option<TypedValue> {
        match value {
            &Value::Boolean(x) => Some(TypedValue::Boolean(x)),
            &Value::Instant(x) => Some(TypedValue::Instant(x)),
            &Value::Integer(x) => Some(TypedValue::Long(x)),
            &Value::Uuid(x) => Some(TypedValue::Uuid(x)),
            &Value::Float(ref x) => Some(TypedValue::Double(x.clone())),
            &Value::Text(ref x) => Some(x.clone().into()),
            &Value::Keyword(ref x) => Some(x.clone().into()),
            _ => None
        }
    }

    /// Return the corresponding SQLite `value` and `value_type_tag` pair.
    fn to_sql_value_pair<'a>(&'a self) -> (ToSqlOutput<'a>, i32) {
        match self {
            &TypedValue::Ref(x) => (rusqlite::types::Value::Integer(x).into(), 0),
            &TypedValue::Boolean(x) => (rusqlite::types::Value::Integer(if x { 1 } else { 0 }).into(), 1),
            &TypedValue::Instant(x) => (rusqlite::types::Value::Integer(x.to_micros()).into(), 4),
            // SQLite distinguishes integral from decimal types, allowing long and double to share a tag.
            &TypedValue::Long(x) => (rusqlite::types::Value::Integer(x).into(), 5),
            &TypedValue::Double(x) => (rusqlite::types::Value::Real(x.into_inner()).into(), 5),
            &TypedValue::String(ref x) => (rusqlite::types::ValueRef::Text(x.as_str()).into(), 10),
            &TypedValue::Uuid(ref u) => (rusqlite::types::Value::Blob(u.as_bytes().to_vec()).into(), 11),
            &TypedValue::Keyword(ref x) => (rusqlite::types::ValueRef::Text(&x.to_string()).into(), 13),
        }
    }

    /// Return the corresponding EML `value` and `value_type` pair.
    fn to_einsteinml_value_pair(&self) -> (Value, ValueType) {
        match self {
            &TypedValue::Ref(x) => (Value::Integer(x), ValueType::Ref),
            &TypedValue::Boolean(x) => (Value::Boolean(x), ValueType::Boolean),
            &TypedValue::Instant(x) => (Value::Instant(x), ValueType::Instant),
            &TypedValue::Long(x) => (Value::Integer(x), ValueType::Long),
            &TypedValue::Double(x) => (Value::Float(x), ValueType::Double),
            &TypedValue::String(ref x) => (Value::Text(x.as_ref().clone()), ValueType::String),
            &TypedValue::Uuid(ref u) => (Value::Uuid(u.clone()), ValueType::Uuid),
            &TypedValue::Keyword(ref x) => (Value::Keyword(x.as_ref().clone()), ValueType::Keyword),
        }
    }
}

/// Read an arbitrary [e a v value_type_tag] materialized view from the given table in the SQL
/// store.
pub(crate) fn read_materialized_view(conn: &rusqlite::Connection, table: &str) -> Result<Vec<(Causetid, Causetid, TypedValue)>> {
    let mut stmt: rusqlite::Statement = conn.prepare(format!("SELECT e, a, v, value_type_tag FROM {}", table).as_str())?;
    let m: Result<Vec<_>> = stmt.query_and_then(
        &[],
        row_to_datom_assertion
    )?.collect();
    m
}

/// Read the partition map materialized view from the given SQL store.
pub fn read_partition_map(conn: &rusqlite::Connection) -> Result<PartitionMap> {
    // An obviously expensive query, but we use it infrequently:
    // - on first start,
    // - while moving timelines,
    // - during sync.
    // First part of the union sprinkles 'allow_excision' into the 'parts' view.
    // Second part of the union takes care of partitions which are known
    // but don't have any transactions.
    let mut stmt: rusqlite::Statement = conn.prepare("
        SELECT
            known_parts.part,
            known_parts.start,
            known_parts.end,
            parts.idx,
            known_parts.allow_excision
        FROM
            parts
        INNER JOIN
            known_parts
        ON parts.part = known_parts.part

        UNION

        SELECT
            part,
            start,
            end,
            start,
            allow_excision
        FROM
            known_parts
        WHERE
            part NOT IN (SELECT part FROM parts)"
    )?;
    let m = stmt.query_and_then(&[], |row| -> Result<(String, Partition)> {
        Ok((row.get_checked(0)?, Partition::new(row.get_checked(1)?, row.get_checked(2)?, row.get_checked(3)?, row.get_checked(4)?)))
    })?.collect();
    m
}

/// Read the solitonid map materialized view from the given SQL store.
pub(crate) fn read_ident_map(conn: &rusqlite::Connection) -> Result<SolitonidMap> {
    let v = read_materialized_view(conn, "idents")?;
    v.into_iter().map(|(e, a, typed_value)| {
        if a != causetids::einsteindb_IDENT {
            bail!(einsteindbErrorKind::NotYetImplemented(format!("bad idents materialized view: expected :einsteineinsteindb/solitonid but got {}", a)));
        }
        if let TypedValue::Keyword(keyword) = typed_value {
            Ok((keyword.as_ref().clone(), e))
        } else {
            bail!(einsteindbErrorKind::NotYetImplemented(format!("bad idents materialized view: expected [causetid :einsteineinsteindb/solitonid keyword] but got [causetid :einsteineinsteindb/solitonid {:?}]", typed_value)));
        }
    }).collect()
}

/// Read the schema materialized view from the given SQL store.
pub(crate) fn read_attribute_map(conn: &rusqlite::Connection) -> Result<AttributeMap> {
    let causetid_triples = read_materialized_view(conn, "schema")?;
    let mut attribute_map = AttributeMap::default();
    spacetime::update_attribute_map_from_causetid_triples(&mut attribute_map, causetid_triples, vec![])?;
    Ok(attribute_map)
}

/// Read the materialized views from the given SQL store and return a einstai `einsteindb` for querying and
/// applying transactions.
pub(crate) fn read_einsteineinsteindb(conn: &rusqlite::Connection) -> Result<einsteindb> {
    let partition_map = read_partition_map(conn)?;
    let ident_map = read_ident_map(conn)?;
    let attribute_map = read_attribute_map(conn)?;
    let schema = Schema::from_ident_map_and_attribute_map(ident_map, attribute_map)?;
    Ok(einsteindb::new(partition_map, schema))
}

/// Internal representation of an [e a v added] datom, ready to be transacted against the store.
pub type Reducedcauset<'a> = (Causetid, Causetid, &'a Attribute, TypedValue, bool);

#[derive(Clone,Debug,Eq,Hash,Ord,PartialOrd,PartialEq)]
pub enum SearchType {
    Exact,
    Inexact,
}

/// `einstaiStoring` will be the trait that encapsulates the storage layer.  It is consumed by the
/// transaction processing layer.
///
/// Right now, the only impleeinstaiion of `einstaiStoring` is the SQLite-specific SQL schema.  In the
/// future, we might consider other SQL engines (perhaps with different fulltext indexing), or
/// entirely different data stores, say ones shaped like key-value stores.
pub trait einstaiStoring {
    /// Given a slice of [a v] lookup-refs, look up the corresponding [e a v] triples.
    ///
    /// It is assumed that the attribute `a` in each lookup-ref is `:einsteineinsteindb/unique`, so that at most one
    /// matching [e a v] triple exists.  (If this is not true, some matching causetid `e` will be
    /// chosen non-deterministically, if one exists.)
    ///
    /// Returns a map &(a, v) -> e, to avoid cloning potentially large values.  The keys of the map
    /// are exactly those (a, v) pairs that have an assertion [e a v] in the store.
    fn resolve_avs<'a>(&self, avs: &'a [&'a AVPair]) -> Result<AVMap<'a>>;

    /// Begin (or prepare) the underlying storage layer for a new einstai transaction.
    ///
    /// Use this to create temporary tables, prepare indices, set pragmas, etc, before the initial
    /// `insert_non_fts_searches` invocation.
    fn begin_tx_application(&self) -> Result<()>;

    // TODO: this is not a reasonable abstraction, but I don't want to really consider non-SQL storage just yet.
    fn insert_non_fts_searches<'a>(&self, causets: &'a [Reducedcauset], search_type: SearchType) -> Result<()>;
    fn insert_fts_searches<'a>(&self, causets: &'a [Reducedcauset], search_type: SearchType) -> Result<()>;

    /// Prepare the underlying storage layer for finalization after a einstai transaction.
    ///
    /// Use this to finalize temporary tables, complete indices, revert pragmas, etc, after the
    /// final `insert_non_fts_searches` invocation.
    fn materialize_einstai_transaction(&self, tx_id: Causetid) -> Result<()>;

    /// Finalize the underlying storage layer after a einstai transaction.
    ///
    /// This is a final step in performing a transaction.
    fn commit_einstai_transaction(&self, tx_id: Causetid) -> Result<()>;

    /// Extract spacetime-related [e a typed_value added] datoms resolved in the last
    /// materialized transaction.
    fn resolved_spacetime_assertions(&self) -> Result<Vec<(Causetid, Causetid, TypedValue, bool)>>;
}

/// Take search rows and complete `temp.search_results`.
///
/// See https://github.com/Whtcorps Inc and EinstAI Inc/einstai/wiki/Transacting:-causet-to-SQL-translation.
fn search(conn: &rusqlite::Connection) -> Result<()> {
    // First is fast, only one table walk: lookup by exact eav.
    // Second is slower, but still only one table walk: lookup old value by ea.
    let s = r#"
      INSERT INTO temp.search_results
      SELECT t.e0, t.a0, t.v0, t.value_type_tag0, t.added0, t.flags0, ':einsteineinsteindb.cardinality/many', d.rowid, d.v
      FROM temp.exact_searches AS t
      LEFT JOIN datoms AS d
      ON t.e0 = d.e AND
         t.a0 = d.a AND
         t.value_type_tag0 = d.value_type_tag AND
         t.v0 = d.v

      UNION ALL

      SELECT t.e0, t.a0, t.v0, t.value_type_tag0, t.added0, t.flags0, ':einsteineinsteindb.cardinality/one', d.rowid, d.v
      FROM temp.inexact_searches AS t
      LEFT JOIN datoms AS d
      ON t.e0 = d.e AND
         t.a0 = d.a"#;

    let mut stmt = conn.prepare_cached(s)?;
    stmt.execute(&[]).context(einsteindbErrorKind::CouldNotSearch)?;
    Ok(())
}

/// Insert the new transaction into the `transactions` table.
///
/// This turns the contents of `search_results` into a new transaction.
///
/// See https://github.com/Whtcorps Inc and EinstAI Inc/einstai/wiki/Transacting:-causet-to-SQL-translation.
fn insert_transaction(conn: &rusqlite::Connection, tx: Causetid) -> Result<()> {
    // einstai follows Datomic and treats its input as a set.  That means it is okay to transact the
    // same [e a v] twice in one transaction.  However, we don't want to represent the transacted
    // datom twice.  Therefore, the transactor unifies repeated datoms, and in addition we add
    // indices to the search inputs and search results to ensure that we don't see repeated datoms
    // at this point.

    let s = r#"
      INSERT INTO timelined_transactions (e, a, v, tx, added, value_type_tag)
      SELECT e0, a0, v0, ?, 1, value_type_tag0
      FROM temp.search_results
      WHERE added0 IS 1 AND ((rid IS NULL) OR ((rid IS NOT NULL) AND (v0 IS NOT v)))"#;

    let mut stmt = conn.prepare_cached(s)?;
    stmt.execute(&[&tx]).context(einsteindbErrorKind::TxInsertFailedToAddMissingDatoms)?;

    let s = r#"
      INSERT INTO timelined_transactions (e, a, v, tx, added, value_type_tag)
      SELECT DISTINCT e0, a0, v, ?, 0, value_type_tag0
      FROM temp.search_results
      WHERE rid IS NOT NULL AND
            ((added0 IS 0) OR
             (added0 IS 1 AND search_type IS ':einsteineinsteindb.cardinality/one' AND v0 IS NOT v))"#;

    let mut stmt = conn.prepare_cached(s)?;
    stmt.execute(&[&tx]).context(einsteindbErrorKind::TxInsertFailedToRetractDatoms)?;

    Ok(())
}

/// Update the contents of the `datoms` materialized view with the new transaction.
///
/// This applies the contents of `search_results` to the `datoms` table (in place).
///
/// See https://github.com/Whtcorps Inc and EinstAI Inc/einstai/wiki/Transacting:-causet-to-SQL-translation.
fn update_datoms(conn: &rusqlite::Connection, tx: Causetid) -> Result<()> {
    // Delete datoms that were retracted, or those that were :einsteineinsteindb.cardinality/one and will be
    // replaced.
    let s = r#"
        WITH ids AS (SELECT rid
                     FROM temp.search_results
                     WHERE rid IS NOT NULL AND
                           ((added0 IS 0) OR
                            (added0 IS 1 AND search_type IS ':einsteineinsteindb.cardinality/one' AND v0 IS NOT v)))
        DELETE FROM datoms WHERE rowid IN ids"#;

    let mut stmt = conn.prepare_cached(s)?;
    stmt.execute(&[]).context(einsteindbErrorKind::DatomsUpdateFailedToRetract)?;

    // Insert datoms that were added and not already present. We also must expand our bitfield into
    // flags.  Since einstai follows Datomic and treats its input as a set, it is okay to transact
    // the same [e a v] twice in one transaction, but we don't want to represent the transacted
    // datom twice in datoms.  The transactor unifies repeated datoms, and in addition we add
    // indices to the search inputs and search results to ensure that we don't see repeated datoms
    // at this point.
    let s = format!(r#"
      INSERT INTO datoms (e, a, v, tx, value_type_tag, index_avet, index_vaet, index_fulltext, unique_value)
      SELECT e0, a0, v0, ?, value_type_tag0,
             flags0 & {} IS NOT 0,
             flags0 & {} IS NOT 0,
             flags0 & {} IS NOT 0,
             flags0 & {} IS NOT 0
      FROM temp.search_results
      WHERE added0 IS 1 AND ((rid IS NULL) OR ((rid IS NOT NULL) AND (v0 IS NOT v)))"#,
      AttributeBitFlags::IndexAVET as u8,
      AttributeBitFlags::IndexVAET as u8,
      AttributeBitFlags::IndexFulltext as u8,
      AttributeBitFlags::UniqueValue as u8);

    let mut stmt = conn.prepare_cached(&s)?;
    stmt.execute(&[&tx]).context(einsteindbErrorKind::DatomsUpdateFailedToAdd)?;
    Ok(())
}

impl einstaiStoring for rusqlite::Connection {
    fn resolve_avs<'a>(&self, avs: &'a [&'a AVPair]) -> Result<AVMap<'a>> {
        // Start search_id's at some identifiable number.
        let initial_search_id = 2000;
        let bindings_per_statement = 4;

        // We map [a v] -> numeric search_id -> e, and then we use the search_id lookups to finally
        // produce the map [a v] -> e.
        //
        // TODO: `collect` into a HashSet so that any (a, v) is resolved at most once.
        let max_vars = self.limit(Limit::SQLITE_LIMIT_VARIABLE_NUMBER) as usize;
        let chunks: itertools::IntoChunks<_> = avs.into_iter().enumerate().chunks(max_vars / 4);

        // We'd like to `flat_map` here, but it's not obvious how to `flat_map` across `Result`.
        // Alternatively, this is a `fold`, and it might be wise to express it as such.
        let results: Result<Vec<Vec<_>>> = chunks.into_iter().map(|chunk| -> Result<Vec<_>> {
            let mut count = 0;

            // We must keep these computed values somewhere to reference them later, so we can't
            // combine this `map` and the subsequent `flat_map`.
            let block: Vec<(i64, i64, ToSqlOutput<'a>, i32)> = chunk.map(|(index, &&(a, ref v))| {
                count += 1;
                let search_id: i64 = initial_search_id + index as i64;
                let (value, value_type_tag) = v.to_sql_value_pair();
                (search_id, a, value, value_type_tag)
            }).collect();

            // `params` reference computed values in `block`.
            let params: Vec<&ToSql> = block.iter().flat_map(|&(ref searchid, ref a, ref value, ref value_type_tag)| {
                // Avoid inner heap allocation.
                once(searchid as &ToSql)
                    .chain(once(a as &ToSql)
                           .chain(once(value as &ToSql)
                                  .chain(once(value_type_tag as &ToSql))))
            }).collect();

            // TODO: cache these statements for selected values of `count`.
            // TODO: query against `datoms` and UNION ALL with `fulltext_datoms` rather than
            // querying against `all_datoms`.  We know all the attributes, and in the common case,
            // where most unique attributes will not be fulltext-indexed, we'll be querying just
            // `datoms`, which will be much faster.ˇ
            assert!(bindings_per_statement * count < max_vars, "Too many values: {} * {} >= {}", bindings_per_statement, count, max_vars);

            let values: String = repeat_values(bindings_per_statement, count);
            let s: String = format!("WITH t(search_id, a, v, value_type_tag) AS (VALUES {}) SELECT t.search_id, d.e \
                                     FROM t, all_datoms AS d \
                                     WHERE d.index_avet IS NOT 0 AND d.a = t.a AND d.value_type_tag = t.value_type_tag AND d.v = t.v",
                                    values);
            let mut stmt: rusqlite::Statement = self.prepare(s.as_str())?;

            let m: Result<Vec<(i64, Causetid)>> = stmt.query_and_then(&params, |row| -> Result<(i64, Causetid)> {
                Ok((row.get_checked(0)?, row.get_checked(1)?))
            })?.collect();
            m
        }).collect::<Result<Vec<Vec<(i64, Causetid)>>>>();

        // Flatten.
        let results: Vec<(i64, Causetid)> = results?.as_slice().concat();

        // Create map [a v] -> e.
        let m: HashMap<&'a AVPair, Causetid> = results.into_iter().map(|(search_id, causetid)| {
            let index: usize = (search_id - initial_search_id) as usize;
            (avs[index], causetid)
        }).collect();
        Ok(m)
    }

    /// Create empty temporary tables for search parameters and search results.
    fn begin_tx_application(&self) -> Result<()> {
        // We can't do this in one shot, since we can't prepare a batch statement.
        let statements = [
            r#"DROP TABLE IF EXISTS temp.exact_searches"#,
            // Note that `flags0` is a bitfield of several flags compressed via
            // `AttributeBitFlags.flags()` in the temporary search tables, later
            // expanded in the `datoms` insertion.
            r#"CREATE TABLE temp.exact_searches (
               e0 INTEGER NOT NULL,
               a0 SMALLINT NOT NULL,
               v0 BLOB NOT NULL,
               value_type_tag0 SMALLINT NOT NULL,
               added0 TINYINT NOT NULL,
               flags0 TINYINT NOT NULL)"#,
            // There's no real need to split exact and inexact searches, so long as we keep things
            // in the correct place and performant.  Splitting has the advantage of being explicit
            // and slightly easier to read, so we'll do that to start.
            r#"DROP TABLE IF EXISTS temp.inexact_searches"#,
            r#"CREATE TABLE temp.inexact_searches (
               e0 INTEGER NOT NULL,
               a0 SMALLINT NOT NULL,
               v0 BLOB NOT NULL,
               value_type_tag0 SMALLINT NOT NULL,
               added0 TINYINT NOT NULL,
               flags0 TINYINT NOT NULL)"#,

            // It is fine to transact the same [e a v] twice in one transaction, but the transaction
            // processor should unify such repeated datoms.  This index will cause insertion to fail
            // if the transaction processor incorrectly tries to assert the same (cardinality one)
            // datom twice.  (Sadly, the failure is opaque.)
            r#"CREATE UNIQUE INDEX IF NOT EXISTS temp.inexact_searches_unique ON inexact_searches (e0, a0) WHERE added0 = 1"#,
            r#"DROP TABLE IF EXISTS temp.search_results"#,
            // TODO: don't encode search_type as a STRING.  This is explicit and much easier to read
            // than another flag, so we'll do it to start, and optimize later.
            r#"CREATE TABLE temp.search_results (
               e0 INTEGER NOT NULL,
               a0 SMALLINT NOT NULL,
               v0 BLOB NOT NULL,
               value_type_tag0 SMALLINT NOT NULL,
               added0 TINYINT NOT NULL,
               flags0 TINYINT NOT NULL,
               search_type STRING NOT NULL,
               rid INTEGER,
               v BLOB)"#,
            // It is fine to transact the same [e a v] twice in one transaction, but the transaction
            // processor should identify those datoms.  This index will cause insertion to fail if
            // the internals of the database searching code incorrectly find the same datom twice.
            // (Sadly, the failure is opaque.)
            //
            // N.b.: temp goes on index name, not table name.  See http://stackoverflow.com/a/22308016.
            r#"CREATE UNIQUE INDEX IF NOT EXISTS temp.search_results_unique ON search_results (e0, a0, v0, value_type_tag0)"#,
        ];

        for statement in &statements {
            let mut stmt = self.prepare_cached(statement)?;
            stmt.execute(&[]).context(einsteindbErrorKind::FailedToCreateTempTables)?;
        }

        Ok(())
    }

    /// Insert search rows into temporary search tables.
    ///
    /// Eventually, the details of this approach will be captured in
    /// https://github.com/Whtcorps Inc and EinstAI Inc/einstai/wiki/Transacting:-causet-to-SQL-translation.
    fn insert_non_fts_searches<'a>(&self, causets: &'a [Reducedcauset<'a>], search_type: SearchType) -> Result<()> {
        let bindings_per_statement = 6;

        let max_vars = self.limit(Limit::SQLITE_LIMIT_VARIABLE_NUMBER) as usize;
        let chunks: itertools::IntoChunks<_> = causets.into_iter().chunks(max_vars / bindings_per_statement);

        // We'd like to flat_map here, but it's not obvious how to flat_map across Result.
        let results: Result<Vec<()>> = chunks.into_iter().map(|chunk| -> Result<()> {
            let mut count = 0;

            // We must keep these computed values somewhere to reference them later, so we can't
            // combine this map and the subsequent flat_map.
            // (e0, a0, v0, value_type_tag0, added0, flags0)
            let block: Result<Vec<(i64 /* e */,
                                   i64 /* a */,
                                   ToSqlOutput<'a> /* value */,
                                   i32 /* value_type_tag */,
                                   bool, /* added0 */
                                   u8 /* flags0 */)>> = chunk.map(|&(e, a, ref attribute, ref typed_value, added)| {
                count += 1;

                // Now we can represent the typed value as an SQL value.
                let (value, value_type_tag): (ToSqlOutput, i32) = typed_value.to_sql_value_pair();

                Ok((e, a, value, value_type_tag, added, attribute.flags()))
            }).collect();
            let block = block?;

            // `params` reference computed values in `block`.
            let params: Vec<&ToSql> = block.iter().flat_map(|&(ref e, ref a, ref value, ref value_type_tag, added, ref flags)| {
                // Avoid inner heap allocation.
                // TODO: extract some finite length iterator to make this less indented!
                once(e as &ToSql)
                    .chain(once(a as &ToSql)
                           .chain(once(value as &ToSql)
                                  .chain(once(value_type_tag as &ToSql)
                                         .chain(once(to_bool_ref(added) as &ToSql)
                                                .chain(once(flags as &ToSql))))))
            }).collect();

            // TODO: cache this for selected values of count.
            assert!(bindings_per_statement * count < max_vars, "Too many values: {} * {} >= {}", bindings_per_statement, count, max_vars);
            let values: String = repeat_values(bindings_per_statement, count);
            let s: String = if search_type == SearchType::Exact {
                format!("INSERT INTO temp.exact_searches (e0, a0, v0, value_type_tag0, added0, flags0) VALUES {}", values)
            } else {
                // This will err for duplicates within the tx.
                format!("INSERT INTO temp.inexact_searches (e0, a0, v0, value_type_tag0, added0, flags0) VALUES {}", values)
            };

            // TODO: consider ensuring we inserted the expected number of rows.
            let mut stmt = self.prepare_cached(s.as_str())?;
            stmt.execute(&params)
                .context(einsteindbErrorKind::NonFtsInsertionIntoTempSearchTableFailed)
                .map_err(|e| e.into())
                .map(|_c| ())
        }).collect::<Result<Vec<()>>>();

        results.map(|_| ())
    }

    /// Insert search rows into temporary search tables.
    ///
    /// Eventually, the details of this approach will be captured in
    /// https://github.com/Whtcorps Inc and EinstAI Inc/einstai/wiki/Transacting:-causet-to-SQL-translation.
    fn insert_fts_searches<'a>(&self, causets: &'a [Reducedcauset<'a>], search_type: SearchType) -> Result<()> {
        let max_vars = self.limit(Limit::SQLITE_LIMIT_VARIABLE_NUMBER) as usize;
        let bindings_per_statement = 6;

        let mut outer_searchid = 2000;

        let chunks: itertools::IntoChunks<_> = causets.into_iter().chunks(max_vars / bindings_per_statement);

        // From string to (searchid, value_type_tag).
        let mut seen: HashMap<ValueRc<String>, (i64, i32)> = HashMap::with_capacity(causets.len());

        // We'd like to flat_map here, but it's not obvious how to flat_map across Result.
        let results: Result<Vec<()>> = chunks.into_iter().map(|chunk| -> Result<()> {
            let mut datom_count = 0;
            let mut string_count = 0;

            // We must keep these computed values somewhere to reference them later, so we can't
            // combine this map and the subsequent flat_map.
            // (e0, a0, v0, value_type_tag0, added0, flags0)
            let block: Result<Vec<(i64 /* e */,
                                   i64 /* a */,
                                   Option<ToSqlOutput<'a>> /* value */,
                                   i32 /* value_type_tag */,
                                   bool /* added0 */,
                                   u8 /* flags0 */,
                                   i64 /* searchid */)>> = chunk.map(|&(e, a, ref attribute, ref typed_value, added)| {
                match typed_value {
                    &TypedValue::String(ref rc) => {
                        datom_count += 1;
                        let entry = seen.entry(rc.clone());
                        match entry {
                            Entry::Occupied(entry) => {
                                let &(searchid, value_type_tag) = entry.get();
                                Ok((e, a, None, value_type_tag, added, attribute.flags(), searchid))
                            },
                            Entry::Vacant(entry) => {
                                outer_searchid += 1;
                                string_count += 1;

                                // Now we can represent the typed value as an SQL value.
                                let (value, value_type_tag): (ToSqlOutput, i32) = typed_value.to_sql_value_pair();
                                entry.insert((outer_searchid, value_type_tag));

                                Ok((e, a, Some(value), value_type_tag, added, attribute.flags(), outer_searchid))
                            }
                        }
                    },
                    _ => {
                        bail!(einsteindbErrorKind::WrongTypeValueForFtsAssertion);
                    },
                }


            }).collect();
            let block = block?;

            // First, insert all fulltext string values.
            // `fts_params` reference computed values in `block`.
            let fts_params: Vec<&ToSql> =
                block.iter()
                     .filter(|&&(ref _e, ref _a, ref value, ref _value_type_tag, _added, ref _flags, ref _searchid)| {
                         value.is_some()
                     })
                     .flat_map(|&(ref _e, ref _a, ref value, ref _value_type_tag, _added, ref _flags, ref searchid)| {
                         // Avoid inner heap allocation.
                         once(value as &ToSql)
                             .chain(once(searchid as &ToSql))
                     }).collect();

            // TODO: make this maximally efficient. It's not terribly inefficient right now.
            let fts_values: String = repeat_values(2, string_count);
            let fts_s: String = format!("INSERT INTO fulltext_values_view (text, searchid) VALUES {}", fts_values);

            // TODO: consider ensuring we inserted the expected number of rows.
            let mut stmt = self.prepare_cached(fts_s.as_str())?;
            stmt.execute(&fts_params).context(einsteindbErrorKind::FtsInsertionFailed)?;

            // Second, insert searches.
            // `params` reference computed values in `block`.
            let params: Vec<&ToSql> = block.iter().flat_map(|&(ref e, ref a, ref _value, ref value_type_tag, added, ref flags, ref searchid)| {
                // Avoid inner heap allocation.
                // TODO: extract some finite length iterator to make this less indented!
                once(e as &ToSql)
                    .chain(once(a as &ToSql)
                           .chain(once(searchid as &ToSql)
                                  .chain(once(value_type_tag as &ToSql)
                                         .chain(once(to_bool_ref(added) as &ToSql)
                                                .chain(once(flags as &ToSql))))))
            }).collect();

            // TODO: cache this for selected values of count.
            assert!(bindings_per_statement * datom_count < max_vars, "Too many values: {} * {} >= {}", bindings_per_statement, datom_count, max_vars);
            let inner = "(?, ?, (SELECT rowid FROM fulltext_values WHERE searchid = ?), ?, ?, ?)".to_string();
            // Like "(?, ?, (SELECT rowid FROM fulltext_values WHERE searchid = ?), ?, ?, ?), (?, ?, (SELECT rowid FROM fulltext_values WHERE searchid = ?), ?, ?, ?)".
            let fts_values: String = repeat(inner).take(datom_count).join(", ");
            let s: String = if search_type == SearchType::Exact {
                format!("INSERT INTO temp.exact_searches (e0, a0, v0, value_type_tag0, added0, flags0) VALUES {}", fts_values)
            } else {
                format!("INSERT INTO temp.inexact_searches (e0, a0, v0, value_type_tag0, added0, flags0) VALUES {}", fts_values)
            };

            // TODO: consider ensuring we inserted the expected number of rows.
            let mut stmt = self.prepare_cached(s.as_str())?;
            stmt.execute(&params).context(einsteindbErrorKind::FtsInsertionIntoTempSearchTableFailed)
                .map_err(|e| e.into())
                .map(|_c| ())
        }).collect::<Result<Vec<()>>>();

        // Finally, clean up temporary searchids.
        let mut stmt = self.prepare_cached("UPDATE fulltext_values SET searchid = NULL WHERE searchid IS NOT NULL")?;
        stmt.execute(&[]).context(einsteindbErrorKind::FtsFailedToDropSearchIds)?;
        results.map(|_| ())
    }

    fn commit_einstai_transaction(&self, tx_id: Causetid) -> Result<()> {
        insert_transaction(&self, tx_id)?;
        Ok(())
    }

    fn materialize_einstai_transaction(&self, tx_id: Causetid) -> Result<()> {
        search(&self)?;
        update_datoms(&self, tx_id)?;
        Ok(())
    }

    fn resolved_spacetime_assertions(&self) ->  Result<Vec<(Causetid, Causetid, TypedValue, bool)>> {
        let sql_stmt = format!(r#"
            SELECT e, a, v, value_type_tag, added FROM
            (
                SELECT e0 as e, a0 as a, v0 as v, value_type_tag0 as value_type_tag, 1 as added
                FROM temp.search_results
                WHERE a0 IN {} AND added0 IS 1 AND ((rid IS NULL) OR
                    ((rid IS NOT NULL) AND (v0 IS NOT v)))

                UNION

                SELECT e0 as e, a0 as a, v, value_type_tag0 as value_type_tag, 0 as added
                FROM temp.search_results
                WHERE a0 in {} AND rid IS NOT NULL AND
                ((added0 IS 0) OR
                    (added0 IS 1 AND search_type IS ':einsteineinsteindb.cardinality/one' AND v0 IS NOT v))

            ) ORDER BY e, a, v, value_type_tag, added"#,
            causetids::METADATA_SQL_LIST.as_str(), causetids::METADATA_SQL_LIST.as_str()
        );

        let mut stmt = self.prepare_cached(&sql_stmt)?;
        let m: Result<Vec<_>> = stmt.query_and_then(
            &[],
            row_to_transaction_assertion
        )?.collect();
        m
    }
}

/// Extract spacetime-related [e a typed_value added] datoms committed in the given transaction.
pub fn committed_spacetime_assertions(conn: &rusqlite::Connection, tx_id: Causetid) -> Result<Vec<(Causetid, Causetid, TypedValue, bool)>> {
    let sql_stmt = format!(r#"
        SELECT e, a, v, value_type_tag, added
        FROM transactions
        WHERE tx = ? AND a IN {}
        ORDER BY e, a, v, value_type_tag, added"#,
        causetids::METADATA_SQL_LIST.as_str()
    );

    let mut stmt = conn.prepare_cached(&sql_stmt)?;
    let m: Result<Vec<_>> = stmt.query_and_then(
        &[&tx_id as &ToSql],
        row_to_transaction_assertion
    )?.collect();
    m
}

/// Takes a row, produces a transaction quadruple.
fn row_to_transaction_assertion(row: &rusqlite::Row) -> Result<(Causetid, Causetid, TypedValue, bool)> {
    Ok((
        row.get_checked(0)?,
        row.get_checked(1)?,
        TypedValue::from_sql_value_pair(row.get_checked(2)?, row.get_checked(3)?)?,
        row.get_checked(4)?
    ))
}

/// Takes a row, produces a datom quadruple.
fn row_to_datom_assertion(row: &rusqlite::Row) -> Result<(Causetid, Causetid, TypedValue)> {
    Ok((
        row.get_checked(0)?,
        row.get_checked(1)?,
        TypedValue::from_sql_value_pair(row.get_checked(2)?, row.get_checked(3)?)?
    ))
}

/// Update the spacetime materialized views based on the given spacetime report.
///
/// This updates the "causetids", "idents", and "schema" materialized views, copying directly from the
/// "datoms" and "transactions" table as appropriate.
pub fn update_spacetime(conn: &rusqlite::Connection, _old_schema: &Schema, new_schema: &Schema, spacetime_report: &spacetime::MetadataReport) -> Result<()>
{
    use spacetime::AttributeAlteration::*;

    // Populate the materialized view directly from datoms (and, potentially in the future,
    // transactions).  This might generalize nicely as we expand the set of materialized views.
    // TODO: consider doing this in fewer SQLite execute() invocations.
    // TODO: use concat! to avoid creating String instances.
    if !spacetime_report.idents_altered.is_empty() {
        // Solitonids is the materialized view of the [causetid :einsteineinsteindb/solitonid solitonid] slice of datoms.
        conn.execute(format!("DELETE FROM idents").as_str(),
                     &[])?;
        conn.execute(format!("INSERT INTO idents SELECT e, a, v, value_type_tag FROM datoms WHERE a IN {}", causetids::IDENTS_SQL_LIST.as_str()).as_str(),
                     &[])?;
    }

    // Populate the materialized view directly from datoms.
    // It's possible that an "solitonid" was removed, along with its attributes.
    // That's not counted as an "alteration" of attributes, so we explicitly check
    // for non-emptiness of 'idents_altered'.

    // TODO expand spacetime report to allow for better signaling for the above.

    if !spacetime_report.attributes_installed.is_empty()
        || !spacetime_report.attributes_altered.is_empty()
        || !spacetime_report.idents_altered.is_empty() {

        conn.execute(format!("DELETE FROM schema").as_str(),
                     &[])?;
        // NB: we're using :einsteineinsteindb/valueType as a placeholder for the entire schema-defining set.
        let s = format!(r#"
            WITH s(e) AS (SELECT e FROM datoms WHERE a = {})
            INSERT INTO schema
            SELECT s.e, a, v, value_type_tag
            FROM datoms, s
            WHERE s.e = datoms.e AND a IN {}
        "#, causetids::einsteindb_VALUE_TYPE, causetids::SCHEMA_SQL_LIST.as_str());
        conn.execute(&s, &[])?;
    }

    let mut index_stmt = conn.prepare("UPDATE datoms SET index_avet = ? WHERE a = ?")?;
    let mut unique_value_stmt = conn.prepare("UPDATE datoms SET unique_value = ? WHERE a = ?")?;
    let mut cardinality_stmt = conn.prepare(r#"
SELECT EXISTS
    (SELECT 1
        FROM datoms AS left, datoms AS right
        WHERE left.a = ? AND
        left.a = right.a AND
        left.e = right.e AND
        left.v <> right.v)"#)?;

    for (&causetid, alterations) in &spacetime_report.attributes_altered {
        let attribute = new_schema.require_attribute_for_causetid(causetid)?;

        for alteration in alterations {
            match alteration {
                &Index => {
                    // This should always succeed.
                    index_stmt.execute(&[&attribute.index, &causetid as &ToSql])?;
                },
                &Unique => {
                    // TODO: This can fail if there are conflicting values; give a more helpful
                    // error message in this case.
                    if unique_value_stmt.execute(&[to_bool_ref(attribute.unique.is_some()), &causetid as &ToSql]).is_err() {
                        match attribute.unique {
                            Some(attribute::Unique::Value) => bail!(einsteindbErrorKind::SchemaAlterationFailed(format!("Cannot alter schema attribute {} to be :einsteineinsteindb.unique/value", causetid))),
                            Some(attribute::Unique::Idcauset) => bail!(einsteindbErrorKind::SchemaAlterationFailed(format!("Cannot alter schema attribute {} to be :einsteineinsteindb.unique/idcauset", causetid))),
                            None => unreachable!(), // This shouldn't happen, even after we support removing :einsteineinsteindb/unique.
                        }
                    }
                },
                &Cardinality => {
                    // We can always go from :einsteineinsteindb.cardinality/one to :einsteineinsteindb.cardinality many.  It's
                    // :einsteineinsteindb.cardinality/many to :einsteineinsteindb.cardinality/one that can fail.
                    //
                    // TODO: improve the failure message.  Perhaps try to mimic what Datomic says in
                    // this case?
                    if !attribute.multival {
                        let mut rows = cardinality_stmt.query(&[&causetid as &ToSql])?;
                        if rows.next().is_some() {
                            bail!(einsteindbErrorKind::SchemaAlterationFailed(format!("Cannot alter schema attribute {} to be :einsteineinsteindb.cardinality/one", causetid)));
                        }
                    }
                },
                &NoHistory | &IsComponent => {
                    // There's no on disk change required for either of these.
                },
            }
        }
    }

    Ok(())
}

impl PartitionMap {
    /// Allocate a single fresh causetid in the given `partition`.
    pub(crate) fn allocate_causetid(&mut self, partition: &str) -> i64 {
        self.allocate_causetids(partition, 1).start
    }

    /// Allocate `n` fresh causetids in the given `partition`.
    pub(crate) fn allocate_causetids(&mut self, partition: &str, n: usize) -> Range<i64> {
        match self.get_mut(partition) {
            Some(partition) => partition.allocate_causetids(n),
            None => panic!("Cannot allocate causetid from unknown partition: {}", partition)
        }
    }

    pub(crate) fn contains_causetid(&self, causetid: Causetid) -> bool {
        self.values().any(|partition| partition.contains_causetid(causetid))
    }
}

#[cfg(test)]
mod tests {
    extern crate env_logger;

    use std::borrow::{
        Borrow,
    };

    use super::*;
    use debug::{TestConn,tempids};
    use einsteinml::{
        self,
        InternSet,
    };
    use einsteinml::causets::{
        OpType,
    };
    use core_traits::{
        attribute,
        KnownCausetid,
    };
    use einsteineinsteindb_core::{
        HasSchema,
        Keyword,
    };
    use einsteineinsteindb_core::util::Either::*;
    use std::collections::{
        BTreeMap,
    };
    use einsteineinsteindb_traits::errors as errors;
    use internal_types::{
        Term,
    };

    fn run_test_add(mut conn: TestConn) {
        // Test inserting :einsteineinsteindb.cardinality/one elements.
        assert_transact!(conn, "[[:einsteineinsteindb/add 100 :einsteineinsteindb.schema/version 1]
                                 [:einsteineinsteindb/add 101 :einsteineinsteindb.schema/version 2]]");
        assert_matches!(conn.last_transaction(),
                        "[[100 :einsteineinsteindb.schema/version 1 ?tx true]
                          [101 :einsteineinsteindb.schema/version 2 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(conn.datoms(),
                       "[[100 :einsteineinsteindb.schema/version 1]
                         [101 :einsteineinsteindb.schema/version 2]]");

        // Test inserting :einsteineinsteindb.cardinality/many elements.
        assert_transact!(conn, "[[:einsteineinsteindb/add 200 :einsteineinsteindb.schema/attribute 100]
                                 [:einsteineinsteindb/add 200 :einsteineinsteindb.schema/attribute 101]]");
        assert_matches!(conn.last_transaction(),
                        "[[200 :einsteineinsteindb.schema/attribute 100 ?tx true]
                          [200 :einsteineinsteindb.schema/attribute 101 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(conn.datoms(),
                        "[[100 :einsteineinsteindb.schema/version 1]
                          [101 :einsteineinsteindb.schema/version 2]
                          [200 :einsteineinsteindb.schema/attribute 100]
                          [200 :einsteineinsteindb.schema/attribute 101]]");

        // Test replacing existing :einsteineinsteindb.cardinality/one elements.
        assert_transact!(conn, "[[:einsteineinsteindb/add 100 :einsteineinsteindb.schema/version 11]
                                 [:einsteineinsteindb/add 101 :einsteineinsteindb.schema/version 22]]");
        assert_matches!(conn.last_transaction(),
                        "[[100 :einsteineinsteindb.schema/version 1 ?tx false]
                          [100 :einsteineinsteindb.schema/version 11 ?tx true]
                          [101 :einsteineinsteindb.schema/version 2 ?tx false]
                          [101 :einsteineinsteindb.schema/version 22 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(conn.datoms(),
                        "[[100 :einsteineinsteindb.schema/version 11]
                          [101 :einsteineinsteindb.schema/version 22]
                          [200 :einsteineinsteindb.schema/attribute 100]
                          [200 :einsteineinsteindb.schema/attribute 101]]");


        // Test that asserting existing :einsteineinsteindb.cardinality/one elements doesn't change the store.
        assert_transact!(conn, "[[:einsteineinsteindb/add 100 :einsteineinsteindb.schema/version 11]
                                 [:einsteineinsteindb/add 101 :einsteineinsteindb.schema/version 22]]");
        assert_matches!(conn.last_transaction(),
                        "[[?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(conn.datoms(),
                        "[[100 :einsteineinsteindb.schema/version 11]
                          [101 :einsteineinsteindb.schema/version 22]
                          [200 :einsteineinsteindb.schema/attribute 100]
                          [200 :einsteineinsteindb.schema/attribute 101]]");


        // Test that asserting existing :einsteineinsteindb.cardinality/many elements doesn't change the store.
        assert_transact!(conn, "[[:einsteineinsteindb/add 200 :einsteineinsteindb.schema/attribute 100]
                                 [:einsteineinsteindb/add 200 :einsteineinsteindb.schema/attribute 101]]");
        assert_matches!(conn.last_transaction(),
                        "[[?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(conn.datoms(),
                        "[[100 :einsteineinsteindb.schema/version 11]
                          [101 :einsteineinsteindb.schema/version 22]
                          [200 :einsteineinsteindb.schema/attribute 100]
                          [200 :einsteineinsteindb.schema/attribute 101]]");
    }

    #[test]
    fn test_add() {
        run_test_add(TestConn::default());
    }

    #[test]
    fn test_tx_assertions() {
        let mut conn = TestConn::default();

        // Test that txInstant can be asserted.
        assert_transact!(conn, "[[:einsteineinsteindb/add (transaction-tx) :einsteineinsteindb/txInstant #inst \"2017-06-16T00:56:41.257Z\"]
                                 [:einsteineinsteindb/add 100 :einsteineinsteindb/solitonid :name/Ivan]
                                 [:einsteineinsteindb/add 101 :einsteineinsteindb/solitonid :name/Petr]]");
        assert_matches!(conn.last_transaction(),
                        "[[100 :einsteineinsteindb/solitonid :name/Ivan ?tx true]
                          [101 :einsteineinsteindb/solitonid :name/Petr ?tx true]
                          [?tx :einsteineinsteindb/txInstant #inst \"2017-06-16T00:56:41.257Z\" ?tx true]]");

        // Test multiple txInstant with different values should fail.
        assert_transact!(conn, "[[:einsteineinsteindb/add (transaction-tx) :einsteineinsteindb/txInstant #inst \"2017-06-16T00:59:11.257Z\"]
                                 [:einsteineinsteindb/add (transaction-tx) :einsteineinsteindb/txInstant #inst \"2017-06-16T00:59:11.752Z\"]
                                 [:einsteineinsteindb/add 102 :einsteineinsteindb/solitonid :name/Vlad]]",
                         Err("schema constraint violation: cardinality conflicts:\n  CardinalityOneAddConflict { e: 268435458, a: 3, vs: {Instant(2017-06-16T00:59:11.257Z), Instant(2017-06-16T00:59:11.752Z)} }\n"));

        // Test multiple txInstants with the same value.
        assert_transact!(conn, "[[:einsteineinsteindb/add (transaction-tx) :einsteineinsteindb/txInstant #inst \"2017-06-16T00:59:11.257Z\"]
                                 [:einsteineinsteindb/add (transaction-tx) :einsteineinsteindb/txInstant #inst \"2017-06-16T00:59:11.257Z\"]
                                 [:einsteineinsteindb/add 103 :einsteineinsteindb/solitonid :name/Dimitri]
                                 [:einsteineinsteindb/add 104 :einsteineinsteindb/solitonid :name/Anton]]");
        assert_matches!(conn.last_transaction(),
                        "[[103 :einsteineinsteindb/solitonid :name/Dimitri ?tx true]
                          [104 :einsteineinsteindb/solitonid :name/Anton ?tx true]
                          [?tx :einsteineinsteindb/txInstant #inst \"2017-06-16T00:59:11.257Z\" ?tx true]]");

        // We need a few attributes to work with.
        assert_transact!(conn, "[[:einsteineinsteindb/add 111 :einsteineinsteindb/solitonid :test/str]
                                 [:einsteineinsteindb/add 111 :einsteineinsteindb/valueType :einsteineinsteindb.type/string]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/solitonid :test/ref]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/valueType :einsteineinsteindb.type/ref]]");

        // Test that we can assert spacetime about the current transaction.
        assert_transact!(conn, "[[:einsteineinsteindb/add (transaction-tx) :test/str \"We want spacetime!\"]]");
        assert_matches!(conn.last_transaction(),
                        "[[?tx :einsteineinsteindb/txInstant ?ms ?tx true]
                          [?tx :test/str \"We want spacetime!\" ?tx true]]");

        // Test that we can use (transaction-tx) as a value.
        assert_transact!(conn, "[[:einsteineinsteindb/add 333 :test/ref (transaction-tx)]]");
        assert_matches!(conn.last_transaction(),
                        "[[333 :test/ref ?tx ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");

        // Test that we type-check properly.  In the value position, (transaction-tx) yields a ref;
        // :einsteineinsteindb/solitonid expects a keyword.
        assert_transact!(conn, "[[:einsteineinsteindb/add 444 :einsteineinsteindb/solitonid (transaction-tx)]]",
                         Err("not yet implemented: Transaction function transaction-tx produced value of type :einsteineinsteindb.type/ref but expected type :einsteineinsteindb.type/keyword"));

        // Test that we can assert spacetime about the current transaction.
        assert_transact!(conn, "[[:einsteineinsteindb/add (transaction-tx) :test/ref (transaction-tx)]]");
        assert_matches!(conn.last_transaction(),
                        "[[?tx :einsteineinsteindb/txInstant ?ms ?tx true]
                          [?tx :test/ref ?tx ?tx true]]");
    }

    #[test]
    fn test_retract() {
        let mut conn = TestConn::default();

        // Insert a few :einsteineinsteindb.cardinality/one elements.
        assert_transact!(conn, "[[:einsteineinsteindb/add 100 :einsteineinsteindb.schema/version 1]
                                 [:einsteineinsteindb/add 101 :einsteineinsteindb.schema/version 2]]");
        assert_matches!(conn.last_transaction(),
                        "[[100 :einsteineinsteindb.schema/version 1 ?tx true]
                          [101 :einsteineinsteindb.schema/version 2 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(conn.datoms(),
                        "[[100 :einsteineinsteindb.schema/version 1]
                          [101 :einsteineinsteindb.schema/version 2]]");

        // And a few :einsteineinsteindb.cardinality/many elements.
        assert_transact!(conn, "[[:einsteineinsteindb/add 200 :einsteineinsteindb.schema/attribute 100]
                                 [:einsteineinsteindb/add 200 :einsteineinsteindb.schema/attribute 101]]");
        assert_matches!(conn.last_transaction(),
                        "[[200 :einsteineinsteindb.schema/attribute 100 ?tx true]
                          [200 :einsteineinsteindb.schema/attribute 101 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(conn.datoms(),
                        "[[100 :einsteineinsteindb.schema/version 1]
                          [101 :einsteineinsteindb.schema/version 2]
                          [200 :einsteineinsteindb.schema/attribute 100]
                          [200 :einsteineinsteindb.schema/attribute 101]]");

        // Test that we can retract :einsteineinsteindb.cardinality/one elements.
        assert_transact!(conn, "[[:einsteineinsteindb/retract 100 :einsteineinsteindb.schema/version 1]]");
        assert_matches!(conn.last_transaction(),
                        "[[100 :einsteineinsteindb.schema/version 1 ?tx false]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(conn.datoms(),
                        "[[101 :einsteineinsteindb.schema/version 2]
                          [200 :einsteineinsteindb.schema/attribute 100]
                          [200 :einsteineinsteindb.schema/attribute 101]]");

        // Test that we can retract :einsteineinsteindb.cardinality/many elements.
        assert_transact!(conn, "[[:einsteineinsteindb/retract 200 :einsteineinsteindb.schema/attribute 100]]");
        assert_matches!(conn.last_transaction(),
                        "[[200 :einsteineinsteindb.schema/attribute 100 ?tx false]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(conn.datoms(),
                        "[[101 :einsteineinsteindb.schema/version 2]
                          [200 :einsteineinsteindb.schema/attribute 101]]");

        // Verify that retracting :einsteineinsteindb.cardinality/{one,many} elements that are not present doesn't
        // change the store.
        assert_transact!(conn, "[[:einsteineinsteindb/retract 100 :einsteineinsteindb.schema/version 1]
                                 [:einsteineinsteindb/retract 200 :einsteineinsteindb.schema/attribute 100]]");
        assert_matches!(conn.last_transaction(),
                        "[[?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(conn.datoms(),
                        "[[101 :einsteineinsteindb.schema/version 2]
                          [200 :einsteineinsteindb.schema/attribute 101]]");
    }

    #[test]
    fn test_einsteineinsteindb_doc_is_not_schema() {
        let mut conn = TestConn::default();

        // Neither transaction below is defining a new attribute.  That is, it's fine to use :einsteineinsteindb/doc
        // to describe any causet in the system, not just attributes.  And in particular, including
        // :einsteineinsteindb/doc shouldn't make the transactor consider the causet a schema attribute.
        assert_transact!(conn, r#"
            [{:einsteineinsteindb/doc "test"}]
        "#);

        assert_transact!(conn, r#"
            [{:einsteineinsteindb/solitonid :test/id :einsteineinsteindb/doc "test"}]
        "#);
    }

    // Unique is required!
    #[test]
    fn test_upsert_issue_538() {
        let mut conn = TestConn::default();
        assert_transact!(conn, "
            [{:einsteineinsteindb/solitonid :person/name
              :einsteineinsteindb/valueType :einsteineinsteindb.type/string
              :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/many}
             {:einsteineinsteindb/solitonid :person/age
              :einsteineinsteindb/valueType :einsteineinsteindb.type/long
              :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/one}
             {:einsteineinsteindb/solitonid :person/email
              :einsteineinsteindb/valueType :einsteineinsteindb.type/string
              :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset
              :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/many}]",
              Err("bad schema assertion: :einsteineinsteindb/unique :einsteineinsteindb/unique_idcauset without :einsteineinsteindb/index true for causetid: 65538"));
    }

    // TODO: don't use :einsteineinsteindb/solitonid to test upserts!
    #[test]
    fn test_upsert_vector() {
        let mut conn = TestConn::default();

        // Insert some :einsteineinsteindb.unique/idcauset elements.
        assert_transact!(conn, "[[:einsteineinsteindb/add 100 :einsteineinsteindb/solitonid :name/Ivan]
                                 [:einsteineinsteindb/add 101 :einsteineinsteindb/solitonid :name/Petr]]");
        assert_matches!(conn.last_transaction(),
                        "[[100 :einsteineinsteindb/solitonid :name/Ivan ?tx true]
                          [101 :einsteineinsteindb/solitonid :name/Petr ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(conn.datoms(),
                        "[[100 :einsteineinsteindb/solitonid :name/Ivan]
                          [101 :einsteineinsteindb/solitonid :name/Petr]]");

        // Upserting two tempids to the same causetid works.
        let report = assert_transact!(conn, "[[:einsteineinsteindb/add \"t1\" :einsteineinsteindb/solitonid :name/Ivan]
                                              [:einsteineinsteindb/add \"t1\" :einsteineinsteindb.schema/attribute 100]
                                              [:einsteineinsteindb/add \"t2\" :einsteineinsteindb/solitonid :name/Petr]
                                              [:einsteineinsteindb/add \"t2\" :einsteineinsteindb.schema/attribute 101]]");
        assert_matches!(conn.last_transaction(),
                        "[[100 :einsteineinsteindb.schema/attribute :name/Ivan ?tx true]
                          [101 :einsteineinsteindb.schema/attribute :name/Petr ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(conn.datoms(),
                        "[[100 :einsteineinsteindb/solitonid :name/Ivan]
                          [100 :einsteineinsteindb.schema/attribute :name/Ivan]
                          [101 :einsteineinsteindb/solitonid :name/Petr]
                          [101 :einsteineinsteindb.schema/attribute :name/Petr]]");
        assert_matches!(tempids(&report),
                        "{\"t1\" 100
                          \"t2\" 101}");

        // Upserting a tempid works.  The ref doesn't have to exist (at this time), but we can't
        // reuse an existing ref due to :einsteineinsteindb/unique :einsteineinsteindb.unique/value.
        let report = assert_transact!(conn, "[[:einsteineinsteindb/add \"t1\" :einsteineinsteindb/solitonid :name/Ivan]
                                              [:einsteineinsteindb/add \"t1\" :einsteineinsteindb.schema/attribute 102]]");
        assert_matches!(conn.last_transaction(),
                        "[[100 :einsteineinsteindb.schema/attribute 102 ?tx true]
                          [?true :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(conn.datoms(),
                        "[[100 :einsteineinsteindb/solitonid :name/Ivan]
                          [100 :einsteineinsteindb.schema/attribute :name/Ivan]
                          [100 :einsteineinsteindb.schema/attribute 102]
                          [101 :einsteineinsteindb/solitonid :name/Petr]
                          [101 :einsteineinsteindb.schema/attribute :name/Petr]]");
        assert_matches!(tempids(&report),
                        "{\"t1\" 100}");

        // A single complex upsert allocates a new causetid.
        let report = assert_transact!(conn, "[[:einsteineinsteindb/add \"t1\" :einsteineinsteindb.schema/attribute \"t2\"]]");
        assert_matches!(conn.last_transaction(),
                        "[[65536 :einsteineinsteindb.schema/attribute 65537 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(tempids(&report),
                        "{\"t1\" 65536
                          \"t2\" 65537}");

        // Conflicting upserts fail.
        assert_transact!(conn, "[[:einsteineinsteindb/add \"t1\" :einsteineinsteindb/solitonid :name/Ivan]
                                 [:einsteineinsteindb/add \"t1\" :einsteineinsteindb/solitonid :name/Petr]]",
                         Err("schema constraint violation: conflicting upserts:\n  tempid External(\"t1\") upserts to {KnownCausetid(100), KnownCausetid(101)}\n"));

        // The error messages of conflicting upserts gives information about all failing upserts (in a particular generation).
        assert_transact!(conn, "[[:einsteineinsteindb/add \"t2\" :einsteineinsteindb/solitonid :name/Grigory]
                                 [:einsteineinsteindb/add \"t2\" :einsteineinsteindb/solitonid :name/Petr]
                                 [:einsteineinsteindb/add \"t2\" :einsteineinsteindb/solitonid :name/Ivan]
                                 [:einsteineinsteindb/add \"t1\" :einsteineinsteindb/solitonid :name/Ivan]
                                 [:einsteineinsteindb/add \"t1\" :einsteineinsteindb/solitonid :name/Petr]]",
                         Err("schema constraint violation: conflicting upserts:\n  tempid External(\"t1\") upserts to {KnownCausetid(100), KnownCausetid(101)}\n  tempid External(\"t2\") upserts to {KnownCausetid(100), KnownCausetid(101)}\n"));

        // tempids in :einsteineinsteindb/retract that don't upsert fail.
        assert_transact!(conn, "[[:einsteineinsteindb/retract \"t1\" :einsteineinsteindb/solitonid :name/Anonymous]]",
                         Err("not yet implemented: [:einsteineinsteindb/retract ...] causet referenced tempid that did not upsert: t1"));

        // tempids in :einsteineinsteindb/retract that do upsert are retracted.  The ref given doesn't exist, so the
        // assertion will be ignored.
        let report = assert_transact!(conn, "[[:einsteineinsteindb/add \"t1\" :einsteineinsteindb/solitonid :name/Ivan]
                                              [:einsteineinsteindb/retract \"t1\" :einsteineinsteindb.schema/attribute 103]]");
        assert_matches!(conn.last_transaction(),
                        "[[?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(tempids(&report),
                        "{\"t1\" 100}");

        // A multistep upsert.  The upsert algorithm will first try to resolve "t1", fail, and then
        // allocate both "t1" and "t2".
        let report = assert_transact!(conn, "[[:einsteineinsteindb/add \"t1\" :einsteineinsteindb/solitonid :name/Josef]
                                              [:einsteineinsteindb/add \"t2\" :einsteineinsteindb.schema/attribute \"t1\"]]");
        assert_matches!(conn.last_transaction(),
                        "[[65538 :einsteineinsteindb/solitonid :name/Josef ?tx true]
                          [65539 :einsteineinsteindb.schema/attribute :name/Josef ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(tempids(&report),
                        "{\"t1\" 65538
                          \"t2\" 65539}");

        // A multistep insert.  This time, we can resolve both, but we have to try "t1", succeed,
        // and then resolve "t2".
        // TODO: We can't quite test this without more schema elements.
        // conn.transact("[[:einsteineinsteindb/add \"t1\" :einsteineinsteindb/solitonid :name/Josef]
        //                 [:einsteineinsteindb/add \"t2\" :einsteineinsteindb/solitonid \"t1\"]]");
        // assert_matches!(conn.last_transaction(),
        //                 "[[65538 :einsteineinsteindb/solitonid :name/Josef]
        //                   [65538 :einsteineinsteindb/solitonid :name/Karl]
        //                   [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
    }

    #[test]
    fn test_resolved_upserts() {
        let mut conn = TestConn::default();
        assert_transact!(conn, "[
            {:einsteineinsteindb/solitonid :test/id
             :einsteineinsteindb/valueType :einsteineinsteindb.type/string
             :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset
             :einsteineinsteindb/index true
             :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/one}
            {:einsteineinsteindb/solitonid :test/ref
             :einsteineinsteindb/valueType :einsteineinsteindb.type/ref
             :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset
             :einsteineinsteindb/index true
             :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/one}
        ]");

        // Partial data for :test/id, links via :test/ref.
        assert_transact!(conn, r#"[
            [:einsteineinsteindb/add 100 :test/id "0"]
            [:einsteineinsteindb/add 101 :test/ref 100]
            [:einsteineinsteindb/add 102 :test/ref 101]
            [:einsteineinsteindb/add 103 :test/ref 102]
        ]"#);

        // Fill in the rest of the data for :test/id, using the links of :test/ref.
        let report = assert_transact!(conn, r#"[
            {:einsteineinsteindb/id "a" :test/id "0"}
            {:einsteineinsteindb/id "b" :test/id "1" :test/ref "a"}
            {:einsteineinsteindb/id "c" :test/id "2" :test/ref "b"}
            {:einsteineinsteindb/id "d" :test/id "3" :test/ref "c"}
        ]"#);

        assert_matches!(tempids(&report), r#"{
            "a" 100
            "b" 101
            "c" 102
            "d" 103
        }"#);

        assert_matches!(conn.last_transaction(), r#"[
            [101 :test/id "1" ?tx true]
            [102 :test/id "2" ?tx true]
            [103 :test/id "3" ?tx true]
            [?tx :einsteineinsteindb/txInstant ?ms ?tx true]
        ]"#);
    }

    #[test]
    fn test_sqlite_limit() {
        let conn = new_connection("").expect("Couldn't open in-memory einsteineinsteindb");
        let initial = conn.limit(Limit::SQLITE_LIMIT_VARIABLE_NUMBER);
        // Sanity check.
        assert!(initial > 500);

        // Make sure setting works.
        conn.set_limit(Limit::SQLITE_LIMIT_VARIABLE_NUMBER, 222);
        assert_eq!(222, conn.limit(Limit::SQLITE_LIMIT_VARIABLE_NUMBER));
    }

    #[test]
    fn test_einsteineinsteindb_install() {
        let mut conn = TestConn::default();

        // We're missing some tests here, since our impleeinstaiion is incomplete.
        // See https://github.com/Whtcorps Inc and EinstAI Inc/einstai/issues/797

        // We can assert a new schema attribute.
        assert_transact!(conn, "[[:einsteineinsteindb/add 100 :einsteineinsteindb/solitonid :test/solitonid]
                                 [:einsteineinsteindb/add 100 :einsteineinsteindb/valueType :einsteineinsteindb.type/long]
                                 [:einsteineinsteindb/add 100 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/many]]");

        assert_eq!(conn.schema.causetid_map.get(&100).cloned().unwrap(), to_namespaced_keyword(":test/solitonid").unwrap());
        assert_eq!(conn.schema.ident_map.get(&to_namespaced_keyword(":test/solitonid").unwrap()).cloned().unwrap(), 100);
        let attribute = conn.schema.attribute_for_causetid(100).unwrap().clone();
        assert_eq!(attribute.value_type, ValueType::Long);
        assert_eq!(attribute.multival, true);
        assert_eq!(attribute.fulltext, false);

        assert_matches!(conn.last_transaction(),
                        "[[100 :einsteineinsteindb/solitonid :test/solitonid ?tx true]
                          [100 :einsteineinsteindb/valueType :einsteineinsteindb.type/long ?tx true]
                          [100 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/many ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(conn.datoms(),
                        "[[100 :einsteineinsteindb/solitonid :test/solitonid]
                          [100 :einsteineinsteindb/valueType :einsteineinsteindb.type/long]
                          [100 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/many]]");

        // Let's check we actually have the schema characteristics we expect.
        let attribute = conn.schema.attribute_for_causetid(100).unwrap().clone();
        assert_eq!(attribute.value_type, ValueType::Long);
        assert_eq!(attribute.multival, true);
        assert_eq!(attribute.fulltext, false);

        // Let's check that we can use the freshly installed attribute.
        assert_transact!(conn, "[[:einsteineinsteindb/add 101 100 -10]
                                 [:einsteineinsteindb/add 101 :test/solitonid -9]]");

        assert_matches!(conn.last_transaction(),
                        "[[101 :test/solitonid -10 ?tx true]
                          [101 :test/solitonid -9 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");

        // Cannot retract a single characteristic of an installed attribute.
        assert_transact!(conn,
                         "[[:einsteineinsteindb/retract 100 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/many]]",
                         Err("bad schema assertion: Retracting attribute 8 for causet 100 not permitted."));

        // Cannot retract a single characteristic of an installed attribute.
        assert_transact!(conn,
                         "[[:einsteineinsteindb/retract 100 :einsteineinsteindb/valueType :einsteineinsteindb.type/long]]",
                         Err("bad schema assertion: Retracting attribute 7 for causet 100 not permitted."));

        // Cannot retract a non-defining set of characteristics of an installed attribute.
        assert_transact!(conn,
                         "[[:einsteineinsteindb/retract 100 :einsteineinsteindb/valueType :einsteineinsteindb.type/long]
                         [:einsteineinsteindb/retract 100 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/many]]",
                         Err("bad schema assertion: Retracting defining attributes of a schema without retracting its :einsteineinsteindb/solitonid is not permitted."));

        // See https://github.com/Whtcorps Inc and EinstAI Inc/einstai/issues/796.
        // assert_transact!(conn,
        //                 "[[:einsteineinsteindb/retract 100 :einsteineinsteindb/solitonid :test/solitonid]]",
        //                 Err("bad schema assertion: Retracting :einsteineinsteindb/solitonid of a schema without retracting its defining attributes is not permitted."));

        // Can retract all of characterists of an installed attribute in one go.
        assert_transact!(conn,
                         "[[:einsteineinsteindb/retract 100 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/many]
                         [:einsteineinsteindb/retract 100 :einsteineinsteindb/valueType :einsteineinsteindb.type/long]
                         [:einsteineinsteindb/retract 100 :einsteineinsteindb/solitonid :test/solitonid]]");

        // Trying to install an attribute without a :einsteineinsteindb/solitonid is allowed.
        assert_transact!(conn, "[[:einsteineinsteindb/add 101 :einsteineinsteindb/valueType :einsteineinsteindb.type/long]
                                 [:einsteineinsteindb/add 101 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/many]]");
    }

    #[test]
    fn test_einsteineinsteindb_alter() {
        let mut conn = TestConn::default();

        // Start by installing a :einsteineinsteindb.cardinality/one attribute.
        assert_transact!(conn, "[[:einsteineinsteindb/add 100 :einsteineinsteindb/solitonid :test/solitonid]
                                 [:einsteineinsteindb/add 100 :einsteineinsteindb/valueType :einsteineinsteindb.type/keyword]
                                 [:einsteineinsteindb/add 100 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/one]]");

        // Trying to alter the :einsteineinsteindb/valueType will fail.
        assert_transact!(conn, "[[:einsteineinsteindb/add 100 :einsteineinsteindb/valueType :einsteineinsteindb.type/long]]",
                         Err("bad schema assertion: Schema alteration for existing attribute with causetid 100 is not valid"));

        // But we can alter the cardinality.
        assert_transact!(conn, "[[:einsteineinsteindb/add 100 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/many]]");

        assert_matches!(conn.last_transaction(),
                        "[[100 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/one ?tx false]
                          [100 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/many ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(conn.datoms(),
                        "[[100 :einsteineinsteindb/solitonid :test/solitonid]
                          [100 :einsteineinsteindb/valueType :einsteineinsteindb.type/keyword]
                          [100 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/many]]");

        // Let's check we actually have the schema characteristics we expect.
        let attribute = conn.schema.attribute_for_causetid(100).unwrap().clone();
        assert_eq!(attribute.value_type, ValueType::Keyword);
        assert_eq!(attribute.multival, true);
        assert_eq!(attribute.fulltext, false);

        // Let's check that we can use the freshly altered attribute's new characteristic.
        assert_transact!(conn, "[[:einsteineinsteindb/add 101 100 :test/value1]
                                 [:einsteineinsteindb/add 101 :test/solitonid :test/value2]]");

        assert_matches!(conn.last_transaction(),
                        "[[101 :test/solitonid :test/value1 ?tx true]
                          [101 :test/solitonid :test/value2 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
    }

    #[test]
    fn test_einsteineinsteindb_ident() {
        let mut conn = TestConn::default();

        // We can assert a new :einsteineinsteindb/solitonid.
        assert_transact!(conn, "[[:einsteineinsteindb/add 100 :einsteineinsteindb/solitonid :name/Ivan]]");
        assert_matches!(conn.last_transaction(),
                        "[[100 :einsteineinsteindb/solitonid :name/Ivan ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(conn.datoms(),
                        "[[100 :einsteineinsteindb/solitonid :name/Ivan]]");
        assert_eq!(conn.schema.causetid_map.get(&100).cloned().unwrap(), to_namespaced_keyword(":name/Ivan").unwrap());
        assert_eq!(conn.schema.ident_map.get(&to_namespaced_keyword(":name/Ivan").unwrap()).cloned().unwrap(), 100);

        // We can re-assert an existing :einsteineinsteindb/solitonid.
        assert_transact!(conn, "[[:einsteineinsteindb/add 100 :einsteineinsteindb/solitonid :name/Ivan]]");
        assert_matches!(conn.last_transaction(),
                        "[[?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(conn.datoms(),
                        "[[100 :einsteineinsteindb/solitonid :name/Ivan]]");
        assert_eq!(conn.schema.causetid_map.get(&100).cloned().unwrap(), to_namespaced_keyword(":name/Ivan").unwrap());
        assert_eq!(conn.schema.ident_map.get(&to_namespaced_keyword(":name/Ivan").unwrap()).cloned().unwrap(), 100);

        // We can alter an existing :einsteineinsteindb/solitonid to have a new keyword.
        assert_transact!(conn, "[[:einsteineinsteindb/add :name/Ivan :einsteineinsteindb/solitonid :name/Petr]]");
        assert_matches!(conn.last_transaction(),
                        "[[100 :einsteineinsteindb/solitonid :name/Ivan ?tx false]
                          [100 :einsteineinsteindb/solitonid :name/Petr ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(conn.datoms(),
                        "[[100 :einsteineinsteindb/solitonid :name/Petr]]");
        // Causetid map is updated.
        assert_eq!(conn.schema.causetid_map.get(&100).cloned().unwrap(), to_namespaced_keyword(":name/Petr").unwrap());
        // Solitonid map contains the new solitonid.
        assert_eq!(conn.schema.ident_map.get(&to_namespaced_keyword(":name/Petr").unwrap()).cloned().unwrap(), 100);
        // Solitonid map no longer contains the old solitonid.
        assert!(conn.schema.ident_map.get(&to_namespaced_keyword(":name/Ivan").unwrap()).is_none());

        // We can re-purpose an old solitonid.
        assert_transact!(conn, "[[:einsteineinsteindb/add 101 :einsteineinsteindb/solitonid :name/Ivan]]");
        assert_matches!(conn.last_transaction(),
                        "[[101 :einsteineinsteindb/solitonid :name/Ivan ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(conn.datoms(),
                        "[[100 :einsteineinsteindb/solitonid :name/Petr]
                          [101 :einsteineinsteindb/solitonid :name/Ivan]]");
        // Causetid map contains both causetids.
        assert_eq!(conn.schema.causetid_map.get(&100).cloned().unwrap(), to_namespaced_keyword(":name/Petr").unwrap());
        assert_eq!(conn.schema.causetid_map.get(&101).cloned().unwrap(), to_namespaced_keyword(":name/Ivan").unwrap());
        // Solitonid map contains the new solitonid.
        assert_eq!(conn.schema.ident_map.get(&to_namespaced_keyword(":name/Petr").unwrap()).cloned().unwrap(), 100);
        // Solitonid map contains the old solitonid, but re-purposed to the new causetid.
        assert_eq!(conn.schema.ident_map.get(&to_namespaced_keyword(":name/Ivan").unwrap()).cloned().unwrap(), 101);

        // We can retract an existing :einsteineinsteindb/solitonid.
        assert_transact!(conn, "[[:einsteineinsteindb/retract :name/Petr :einsteineinsteindb/solitonid :name/Petr]]");
        // It's really gone.
        assert!(conn.schema.causetid_map.get(&100).is_none());
        assert!(conn.schema.ident_map.get(&to_namespaced_keyword(":name/Petr").unwrap()).is_none());
    }

    #[test]
    fn test_einsteineinsteindb_alter_cardinality() {
        let mut conn = TestConn::default();

        // Start by installing a :einsteineinsteindb.cardinality/one attribute.
        assert_transact!(conn, "[[:einsteineinsteindb/add 100 :einsteineinsteindb/solitonid :test/solitonid]
                                 [:einsteineinsteindb/add 100 :einsteineinsteindb/valueType :einsteineinsteindb.type/long]
                                 [:einsteineinsteindb/add 100 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/one]]");

        assert_transact!(conn, "[[:einsteineinsteindb/add 200 :test/solitonid 1]]");

        // We can always go from :einsteineinsteindb.cardinality/one to :einsteineinsteindb.cardinality/many.
        assert_transact!(conn, "[[:einsteineinsteindb/add 100 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/many]]");

        assert_transact!(conn, "[[:einsteineinsteindb/add 200 :test/solitonid 2]]");

        assert_matches!(conn.datoms(),
                        "[[100 :einsteineinsteindb/solitonid :test/solitonid]
                          [100 :einsteineinsteindb/valueType :einsteineinsteindb.type/long]
                          [100 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/many]
                          [200 :test/solitonid 1]
                          [200 :test/solitonid 2]]");

        // We can't always go from :einsteineinsteindb.cardinality/many to :einsteineinsteindb.cardinality/one.
        assert_transact!(conn, "[[:einsteineinsteindb/add 100 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/one]]",
                         // TODO: give more helpful error details.
                         Err("schema alteration failed: Cannot alter schema attribute 100 to be :einsteineinsteindb.cardinality/one"));
    }

    #[test]
    fn test_einsteineinsteindb_alter_unique_value() {
        let mut conn = TestConn::default();

        // Start by installing a :einsteineinsteindb.cardinality/one attribute.
        assert_transact!(conn, "[[:einsteineinsteindb/add 100 :einsteineinsteindb/solitonid :test/solitonid]
                                 [:einsteineinsteindb/add 100 :einsteineinsteindb/valueType :einsteineinsteindb.type/long]
                                 [:einsteineinsteindb/add 100 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/one]]");

        assert_transact!(conn, "[[:einsteineinsteindb/add 200 :test/solitonid 1]
                                 [:einsteineinsteindb/add 201 :test/solitonid 1]]");

        // We can't always migrate to be :einsteineinsteindb.unique/value.
        assert_transact!(conn, "[[:einsteineinsteindb/add :test/solitonid :einsteineinsteindb/unique :einsteineinsteindb.unique/value]]",
                         // TODO: give more helpful error details.
                         Err("schema alteration failed: Cannot alter schema attribute 100 to be :einsteineinsteindb.unique/value"));

        // Not even indirectly!
        assert_transact!(conn, "[[:einsteineinsteindb/add :test/solitonid :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset]]",
                         // TODO: give more helpful error details.
                         Err("schema alteration failed: Cannot alter schema attribute 100 to be :einsteineinsteindb.unique/idcauset"));

        // But we can if we make sure there's no repeated [a v] pair.
        assert_transact!(conn, "[[:einsteineinsteindb/add 201 :test/solitonid 2]]");

        assert_transact!(conn, "[[:einsteineinsteindb/add :test/solitonid :einsteineinsteindb/index true]
                                 [:einsteineinsteindb/add :test/solitonid :einsteineinsteindb/unique :einsteineinsteindb.unique/value]
                                 [:einsteineinsteindb/add :einsteineinsteindb.part/einsteineinsteindb :einsteineinsteindb.alter/attribute 100]]");

        // We can also retract the uniqueness constraint altogether.
        assert_transact!(conn, "[[:einsteineinsteindb/retract :test/solitonid :einsteineinsteindb/unique :einsteineinsteindb.unique/value]]");

        // Once we've done so, the schema shows it's not unique…
        {
            let attr = conn.schema.attribute_for_ident(&Keyword::namespaced("test", "solitonid")).unwrap().0;
            assert_eq!(None, attr.unique);
        }

        // … and we can add more assertions with duplicate values.
        assert_transact!(conn, "[[:einsteineinsteindb/add 121 :test/solitonid 1]
                                 [:einsteineinsteindb/add 221 :test/solitonid 2]]");
    }

    #[test]
    fn test_einsteineinsteindb_double_retraction_issue_818() {
        let mut conn = TestConn::default();

        // Start by installing a :einsteineinsteindb.cardinality/one attribute.
        assert_transact!(conn, "[[:einsteineinsteindb/add 100 :einsteineinsteindb/solitonid :test/solitonid]
                                 [:einsteineinsteindb/add 100 :einsteineinsteindb/valueType :einsteineinsteindb.type/string]
                                 [:einsteineinsteindb/add 100 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/one]
                                 [:einsteineinsteindb/add 100 :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset]
                                 [:einsteineinsteindb/add 100 :einsteineinsteindb/index true]]");

        assert_transact!(conn, "[[:einsteineinsteindb/add 200 :test/solitonid \"Oi\"]]");

        assert_transact!(conn, "[[:einsteineinsteindb/add 200 :test/solitonid \"Ai!\"]
                                 [:einsteineinsteindb/retract 200 :test/solitonid \"Oi\"]]");

        assert_matches!(conn.last_transaction(),
                        "[[200 :test/solitonid \"Ai!\" ?tx true]
                          [200 :test/solitonid \"Oi\" ?tx false]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");

        assert_matches!(conn.datoms(),
                        "[[100 :einsteineinsteindb/solitonid :test/solitonid]
                          [100 :einsteineinsteindb/valueType :einsteineinsteindb.type/string]
                          [100 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/one]
                          [100 :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset]
                          [100 :einsteineinsteindb/index true]
                          [200 :test/solitonid \"Ai!\"]]");
    }

    /// Verify that we can't alter :einsteineinsteindb/fulltext schema characteristics at all.
    #[test]
    fn test_einsteineinsteindb_alter_fulltext() {
        let mut conn = TestConn::default();

        // Start by installing a :einsteineinsteindb/fulltext true and a :einsteineinsteindb/fulltext unset attribute.
        assert_transact!(conn, "[[:einsteineinsteindb/add 111 :einsteineinsteindb/solitonid :test/fulltext]
                                 [:einsteineinsteindb/add 111 :einsteineinsteindb/valueType :einsteineinsteindb.type/string]
                                 [:einsteineinsteindb/add 111 :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset]
                                 [:einsteineinsteindb/add 111 :einsteineinsteindb/index true]
                                 [:einsteineinsteindb/add 111 :einsteineinsteindb/fulltext true]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/solitonid :test/string]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/one]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/valueType :einsteineinsteindb.type/string]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/index true]]");

        assert_transact!(conn,
                         "[[:einsteineinsteindb/retract 111 :einsteineinsteindb/fulltext true]]",
                         Err("bad schema assertion: Retracting attribute 12 for causet 111 not permitted."));

        assert_transact!(conn,
                         "[[:einsteineinsteindb/add 222 :einsteineinsteindb/fulltext true]]",
                         Err("bad schema assertion: Schema alteration for existing attribute with causetid 222 is not valid"));
    }

    #[test]
    fn test_einsteineinsteindb_fulltext() {
        let mut conn = TestConn::default();

        // Start by installing a few :einsteineinsteindb/fulltext true attributes.
        assert_transact!(conn, "[[:einsteineinsteindb/add 111 :einsteineinsteindb/solitonid :test/fulltext]
                                 [:einsteineinsteindb/add 111 :einsteineinsteindb/valueType :einsteineinsteindb.type/string]
                                 [:einsteineinsteindb/add 111 :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset]
                                 [:einsteineinsteindb/add 111 :einsteineinsteindb/index true]
                                 [:einsteineinsteindb/add 111 :einsteineinsteindb/fulltext true]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/solitonid :test/other]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/one]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/valueType :einsteineinsteindb.type/string]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/index true]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/fulltext true]]");

        // Let's check we actually have the schema characteristics we expect.
        let fulltext = conn.schema.attribute_for_causetid(111).cloned().expect(":test/fulltext");
        assert_eq!(fulltext.value_type, ValueType::String);
        assert_eq!(fulltext.fulltext, true);
        assert_eq!(fulltext.multival, false);
        assert_eq!(fulltext.unique, Some(attribute::Unique::Idcauset));

        let other = conn.schema.attribute_for_causetid(222).cloned().expect(":test/other");
        assert_eq!(other.value_type, ValueType::String);
        assert_eq!(other.fulltext, true);
        assert_eq!(other.multival, false);
        assert_eq!(other.unique, None);

        // We can add fulltext indexed datoms.
        assert_transact!(conn, "[[:einsteineinsteindb/add 301 :test/fulltext \"test this\"]]");
        // value column is rowid into fulltext table.
        assert_matches!(conn.fulltext_values(),
                        "[[1 \"test this\"]]");
        assert_matches!(conn.last_transaction(),
                        "[[301 :test/fulltext 1 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(conn.datoms(),
                        "[[111 :einsteineinsteindb/solitonid :test/fulltext]
                          [111 :einsteineinsteindb/valueType :einsteineinsteindb.type/string]
                          [111 :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset]
                          [111 :einsteineinsteindb/index true]
                          [111 :einsteineinsteindb/fulltext true]
                          [222 :einsteineinsteindb/solitonid :test/other]
                          [222 :einsteineinsteindb/valueType :einsteineinsteindb.type/string]
                          [222 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/one]
                          [222 :einsteineinsteindb/index true]
                          [222 :einsteineinsteindb/fulltext true]
                          [301 :test/fulltext 1]]");

        // We can replace existing fulltext indexed datoms.
        assert_transact!(conn, "[[:einsteineinsteindb/add 301 :test/fulltext \"alternate thing\"]]");
        // value column is rowid into fulltext table.
        assert_matches!(conn.fulltext_values(),
                        "[[1 \"test this\"]
                          [2 \"alternate thing\"]]");
        assert_matches!(conn.last_transaction(),
                        "[[301 :test/fulltext 1 ?tx false]
                          [301 :test/fulltext 2 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(conn.datoms(),
                        "[[111 :einsteineinsteindb/solitonid :test/fulltext]
                          [111 :einsteineinsteindb/valueType :einsteineinsteindb.type/string]
                          [111 :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset]
                          [111 :einsteineinsteindb/index true]
                          [111 :einsteineinsteindb/fulltext true]
                          [222 :einsteineinsteindb/solitonid :test/other]
                          [222 :einsteineinsteindb/valueType :einsteineinsteindb.type/string]
                          [222 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/one]
                          [222 :einsteineinsteindb/index true]
                          [222 :einsteineinsteindb/fulltext true]
                          [301 :test/fulltext 2]]");

        // We can upsert keyed by fulltext indexed datoms.
        assert_transact!(conn, "[[:einsteineinsteindb/add \"t\" :test/fulltext \"alternate thing\"]
                                 [:einsteineinsteindb/add \"t\" :test/other \"other\"]]");
        // value column is rowid into fulltext table.
        assert_matches!(conn.fulltext_values(),
                        "[[1 \"test this\"]
                          [2 \"alternate thing\"]
                          [3 \"other\"]]");
        assert_matches!(conn.last_transaction(),
                        "[[301 :test/other 3 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(conn.datoms(),
                        "[[111 :einsteineinsteindb/solitonid :test/fulltext]
                          [111 :einsteineinsteindb/valueType :einsteineinsteindb.type/string]
                          [111 :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset]
                          [111 :einsteineinsteindb/index true]
                          [111 :einsteineinsteindb/fulltext true]
                          [222 :einsteineinsteindb/solitonid :test/other]
                          [222 :einsteineinsteindb/valueType :einsteineinsteindb.type/string]
                          [222 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/one]
                          [222 :einsteineinsteindb/index true]
                          [222 :einsteineinsteindb/fulltext true]
                          [301 :test/fulltext 2]
                          [301 :test/other 3]]");

        // We can re-use fulltext values; they won't be added to the fulltext values table twice.
        assert_transact!(conn, "[[:einsteineinsteindb/add 302 :test/other \"alternate thing\"]]");
        // value column is rowid into fulltext table.
        assert_matches!(conn.fulltext_values(),
                        "[[1 \"test this\"]
                          [2 \"alternate thing\"]
                          [3 \"other\"]]");
        assert_matches!(conn.last_transaction(),
                        "[[302 :test/other 2 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(conn.datoms(),
                        "[[111 :einsteineinsteindb/solitonid :test/fulltext]
                          [111 :einsteineinsteindb/valueType :einsteineinsteindb.type/string]
                          [111 :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset]
                          [111 :einsteineinsteindb/index true]
                          [111 :einsteineinsteindb/fulltext true]
                          [222 :einsteineinsteindb/solitonid :test/other]
                          [222 :einsteineinsteindb/valueType :einsteineinsteindb.type/string]
                          [222 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/one]
                          [222 :einsteineinsteindb/index true]
                          [222 :einsteineinsteindb/fulltext true]
                          [301 :test/fulltext 2]
                          [301 :test/other 3]
                          [302 :test/other 2]]");

        // We can retract fulltext indexed datoms.  The underlying fulltext value remains -- indeed,
        // it might still be in use.
        assert_transact!(conn, "[[:einsteineinsteindb/retract 302 :test/other \"alternate thing\"]]");
        // value column is rowid into fulltext table.
        assert_matches!(conn.fulltext_values(),
                        "[[1 \"test this\"]
                          [2 \"alternate thing\"]
                          [3 \"other\"]]");
        assert_matches!(conn.last_transaction(),
                        "[[302 :test/other 2 ?tx false]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(conn.datoms(),
                        "[[111 :einsteineinsteindb/solitonid :test/fulltext]
                          [111 :einsteineinsteindb/valueType :einsteineinsteindb.type/string]
                          [111 :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset]
                          [111 :einsteineinsteindb/index true]
                          [111 :einsteineinsteindb/fulltext true]
                          [222 :einsteineinsteindb/solitonid :test/other]
                          [222 :einsteineinsteindb/valueType :einsteineinsteindb.type/string]
                          [222 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/one]
                          [222 :einsteineinsteindb/index true]
                          [222 :einsteineinsteindb/fulltext true]
                          [301 :test/fulltext 2]
                          [301 :test/other 3]]");
    }

    #[test]
    fn test_lookup_refs_causet_column() {
        let mut conn = TestConn::default();

        // Start by installing a few attributes.
        assert_transact!(conn, "[[:einsteineinsteindb/add 111 :einsteineinsteindb/solitonid :test/unique_value]
                                 [:einsteineinsteindb/add 111 :einsteineinsteindb/valueType :einsteineinsteindb.type/string]
                                 [:einsteineinsteindb/add 111 :einsteineinsteindb/unique :einsteineinsteindb.unique/value]
                                 [:einsteineinsteindb/add 111 :einsteineinsteindb/index true]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/solitonid :test/unique_idcauset]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/valueType :einsteineinsteindb.type/long]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/index true]
                                 [:einsteineinsteindb/add 333 :einsteineinsteindb/solitonid :test/not_unique]
                                 [:einsteineinsteindb/add 333 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/one]
                                 [:einsteineinsteindb/add 333 :einsteineinsteindb/valueType :einsteineinsteindb.type/keyword]
                                 [:einsteineinsteindb/add 333 :einsteineinsteindb/index true]]");

        // And a few datoms to match against.
        assert_transact!(conn, "[[:einsteineinsteindb/add 501 :test/unique_value \"test this\"]
                                 [:einsteineinsteindb/add 502 :test/unique_value \"other\"]
                                 [:einsteineinsteindb/add 503 :test/unique_idcauset -10]
                                 [:einsteineinsteindb/add 504 :test/unique_idcauset -20]
                                 [:einsteineinsteindb/add 505 :test/not_unique :test/keyword]
                                 [:einsteineinsteindb/add 506 :test/not_unique :test/keyword]]");

        // We can resolve lookup refs in the causet column, referring to the attribute as an causetid or an solitonid.
        assert_transact!(conn, "[[:einsteineinsteindb/add (lookup-ref :test/unique_value \"test this\") :test/not_unique :test/keyword]
                                 [:einsteineinsteindb/add (lookup-ref 111 \"other\") :test/not_unique :test/keyword]
                                 [:einsteineinsteindb/add (lookup-ref :test/unique_idcauset -10) :test/not_unique :test/keyword]
                                 [:einsteineinsteindb/add (lookup-ref 222 -20) :test/not_unique :test/keyword]]");
        assert_matches!(conn.last_transaction(),
                        "[[501 :test/not_unique :test/keyword ?tx true]
                          [502 :test/not_unique :test/keyword ?tx true]
                          [503 :test/not_unique :test/keyword ?tx true]
                          [504 :test/not_unique :test/keyword ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");

        // We cannot resolve lookup refs that aren't :einsteineinsteindb/unique.
        assert_transact!(conn,
                         "[[:einsteineinsteindb/add (lookup-ref :test/not_unique :test/keyword) :test/not_unique :test/keyword]]",
                         Err("not yet implemented: Cannot resolve (lookup-ref 333 Keyword(Keyword(NamespaceableName { namespace: Some(\"test\"), name: \"keyword\" }))) with attribute that is not :einsteineinsteindb/unique"));

        // We type check the lookup ref's value against the lookup ref's attribute.
        assert_transact!(conn,
                         "[[:einsteineinsteindb/add (lookup-ref :test/unique_value :test/not_a_string) :test/not_unique :test/keyword]]",
                         Err("value \':test/not_a_string\' is not the expected einstai value type String"));

        // Each lookup ref in the causet column must resolve
        assert_transact!(conn,
                         "[[:einsteineinsteindb/add (lookup-ref :test/unique_value \"unmatched string value\") :test/not_unique :test/keyword]]",
                         Err("no causetid found for solitonid: couldn\'t lookup [a v]: (111, String(\"unmatched string value\"))"));
    }

    #[test]
    fn test_lookup_refs_value_column() {
        let mut conn = TestConn::default();

        // Start by installing a few attributes.
        assert_transact!(conn, "[[:einsteineinsteindb/add 111 :einsteineinsteindb/solitonid :test/unique_value]
                                 [:einsteineinsteindb/add 111 :einsteineinsteindb/valueType :einsteineinsteindb.type/string]
                                 [:einsteineinsteindb/add 111 :einsteineinsteindb/unique :einsteineinsteindb.unique/value]
                                 [:einsteineinsteindb/add 111 :einsteineinsteindb/index true]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/solitonid :test/unique_idcauset]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/valueType :einsteineinsteindb.type/long]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/index true]
                                 [:einsteineinsteindb/add 333 :einsteineinsteindb/solitonid :test/not_unique]
                                 [:einsteineinsteindb/add 333 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/one]
                                 [:einsteineinsteindb/add 333 :einsteineinsteindb/valueType :einsteineinsteindb.type/keyword]
                                 [:einsteineinsteindb/add 333 :einsteineinsteindb/index true]
                                 [:einsteineinsteindb/add 444 :einsteineinsteindb/solitonid :test/ref]
                                 [:einsteineinsteindb/add 444 :einsteineinsteindb/valueType :einsteineinsteindb.type/ref]
                                 [:einsteineinsteindb/add 444 :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset]
                                 [:einsteineinsteindb/add 444 :einsteineinsteindb/index true]]");

        // And a few datoms to match against.
        assert_transact!(conn, "[[:einsteineinsteindb/add 501 :test/unique_value \"test this\"]
                                 [:einsteineinsteindb/add 502 :test/unique_value \"other\"]
                                 [:einsteineinsteindb/add 503 :test/unique_idcauset -10]
                                 [:einsteineinsteindb/add 504 :test/unique_idcauset -20]
                                 [:einsteineinsteindb/add 505 :test/not_unique :test/keyword]
                                 [:einsteineinsteindb/add 506 :test/not_unique :test/keyword]]");

        // We can resolve lookup refs in the causet column, referring to the attribute as an causetid or an solitonid.
        assert_transact!(conn, "[[:einsteineinsteindb/add 601 :test/ref (lookup-ref :test/unique_value \"test this\")]
                                 [:einsteineinsteindb/add 602 :test/ref (lookup-ref 111 \"other\")]
                                 [:einsteineinsteindb/add 603 :test/ref (lookup-ref :test/unique_idcauset -10)]
                                 [:einsteineinsteindb/add 604 :test/ref (lookup-ref 222 -20)]]");
        assert_matches!(conn.last_transaction(),
                        "[[601 :test/ref 501 ?tx true]
                          [602 :test/ref 502 ?tx true]
                          [603 :test/ref 503 ?tx true]
                          [604 :test/ref 504 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");

        // We cannot resolve lookup refs for attributes that aren't :einsteineinsteindb/ref.
        assert_transact!(conn,
                         "[[:einsteineinsteindb/add \"t\" :test/not_unique (lookup-ref :test/unique_value \"test this\")]]",
                         Err("not yet implemented: Cannot resolve value lookup ref for attribute 333 that is not :einsteineinsteindb/valueType :einsteineinsteindb.type/ref"));

        // If a value column lookup ref resolves, we can upsert against it.  Here, the lookup ref
        // resolves to 501, which upserts "t" to 601.
        assert_transact!(conn, "[[:einsteineinsteindb/add \"t\" :test/ref (lookup-ref :test/unique_value \"test this\")]
                                 [:einsteineinsteindb/add \"t\" :test/not_unique :test/keyword]]");
        assert_matches!(conn.last_transaction(),
                        "[[601 :test/not_unique :test/keyword ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");

        // Each lookup ref in the value column must resolve
        assert_transact!(conn,
                         "[[:einsteineinsteindb/add \"t\" :test/ref (lookup-ref :test/unique_value \"unmatched string value\")]]",
                         Err("no causetid found for solitonid: couldn\'t lookup [a v]: (111, String(\"unmatched string value\"))"));
    }

    #[test]
    fn test_explode_value_lists() {
        let mut conn = TestConn::default();

        // Start by installing a few attributes.
        assert_transact!(conn, "[[:einsteineinsteindb/add 111 :einsteineinsteindb/solitonid :test/many]
                                 [:einsteineinsteindb/add 111 :einsteineinsteindb/valueType :einsteineinsteindb.type/long]
                                 [:einsteineinsteindb/add 111 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/many]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/solitonid :test/one]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/valueType :einsteineinsteindb.type/long]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/one]]");

        // Check that we can explode vectors for :einsteineinsteindb.cardinality/many attributes.
        assert_transact!(conn, "[[:einsteineinsteindb/add 501 :test/many [1]]
                                 [:einsteineinsteindb/add 502 :test/many [2 3]]
                                 [:einsteineinsteindb/add 503 :test/many [4 5 6]]]");
        assert_matches!(conn.last_transaction(),
                        "[[501 :test/many 1 ?tx true]
                          [502 :test/many 2 ?tx true]
                          [502 :test/many 3 ?tx true]
                          [503 :test/many 4 ?tx true]
                          [503 :test/many 5 ?tx true]
                          [503 :test/many 6 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");

        // Check that we can explode nested vectors for :einsteineinsteindb.cardinality/many attributes.
        assert_transact!(conn, "[[:einsteineinsteindb/add 600 :test/many [1 [2] [[3] [4]] []]]]");
        assert_matches!(conn.last_transaction(),
                        "[[600 :test/many 1 ?tx true]
                          [600 :test/many 2 ?tx true]
                          [600 :test/many 3 ?tx true]
                          [600 :test/many 4 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");

        // Check that we cannot explode vectors for :einsteineinsteindb.cardinality/one attributes.
        assert_transact!(conn,
                         "[[:einsteineinsteindb/add 501 :test/one [1]]]",
                         Err("not yet implemented: Cannot explode vector value for attribute 222 that is not :einsteineinsteindb.cardinality :einsteineinsteindb.cardinality/many"));
        assert_transact!(conn,
                         "[[:einsteineinsteindb/add 501 :test/one [2 3]]]",
                         Err("not yet implemented: Cannot explode vector value for attribute 222 that is not :einsteineinsteindb.cardinality :einsteineinsteindb.cardinality/many"));
    }

    #[test]
    fn test_explode_map_notation() {
        let mut conn = TestConn::default();

        // Start by installing a few attributes.
        assert_transact!(conn, "[[:einsteineinsteindb/add 111 :einsteineinsteindb/solitonid :test/many]
                                 [:einsteineinsteindb/add 111 :einsteineinsteindb/valueType :einsteineinsteindb.type/long]
                                 [:einsteineinsteindb/add 111 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/many]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/solitonid :test/component]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/isComponent true]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/valueType :einsteineinsteindb.type/ref]
                                 [:einsteineinsteindb/add 333 :einsteineinsteindb/solitonid :test/unique]
                                 [:einsteineinsteindb/add 333 :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset]
                                 [:einsteineinsteindb/add 333 :einsteineinsteindb/index true]
                                 [:einsteineinsteindb/add 333 :einsteineinsteindb/valueType :einsteineinsteindb.type/long]
                                 [:einsteineinsteindb/add 444 :einsteineinsteindb/solitonid :test/dangling]
                                 [:einsteineinsteindb/add 444 :einsteineinsteindb/valueType :einsteineinsteindb.type/ref]]");

        // Check that we can explode map notation without :einsteineinsteindb/id.
        let report = assert_transact!(conn, "[{:test/many 1}]");
        assert_matches!(conn.last_transaction(),
                        "[[?e :test/many 1 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(tempids(&report),
                        "{}");

        // Check that we can explode map notation with :einsteineinsteindb/id, as an causetid, solitonid, and tempid.
        let report = assert_transact!(conn, "[{:einsteineinsteindb/id :einsteineinsteindb/solitonid :test/many 1}
                                              {:einsteineinsteindb/id 500 :test/many 2}
                                              {:einsteineinsteindb/id \"t\" :test/many 3}]");
        assert_matches!(conn.last_transaction(),
                        "[[1 :test/many 1 ?tx true]
                          [500 :test/many 2 ?tx true]
                          [?e :test/many 3 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(tempids(&report),
                        "{\"t\" 65537}");

        // Check that we can explode map notation with :einsteineinsteindb/id as a lookup-ref or tx-function.
        let report = assert_transact!(conn, "[{:einsteineinsteindb/id (lookup-ref :einsteineinsteindb/solitonid :einsteineinsteindb/solitonid) :test/many 4}
                                              {:einsteineinsteindb/id (transaction-tx) :test/many 5}]");
        assert_matches!(conn.last_transaction(),
                        "[[1 :test/many 4 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]
                          [?tx :test/many 5 ?tx true]]");
        assert_matches!(tempids(&report),
                        "{}");

        // Check that we can explode map notation with nested vector values.
        let report = assert_transact!(conn, "[{:test/many [1 2]}]");
        assert_matches!(conn.last_transaction(),
                        "[[?e :test/many 1 ?tx true]
                          [?e :test/many 2 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(tempids(&report),
                        "{}");

        // Check that we can explode map notation with nested maps if the attribute is
        // :einsteineinsteindb/isComponent true.
        let report = assert_transact!(conn, "[{:test/component {:test/many 1}}]");
        assert_matches!(conn.last_transaction(),
                        "[[?e :test/component ?f ?tx true]
                          [?f :test/many 1 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(tempids(&report),
                        "{}");

        // Check that we can explode map notation with nested maps if the inner map contains a
        // :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset attribute.
        let report = assert_transact!(conn, "[{:test/dangling {:test/unique 10}}]");
        assert_matches!(conn.last_transaction(),
                        "[[?e :test/dangling ?f ?tx true]
                          [?f :test/unique 10 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(tempids(&report),
                        "{}");

        // Verify that we can't explode map notation with nested maps if the inner map would be
        // dangling.
        assert_transact!(conn,
                         "[{:test/dangling {:test/many 11}}]",
                         Err("not yet implemented: Cannot explode nested map value that would lead to dangling causet for attribute 444"));

        // Verify that we can explode map notation with nested maps, even if the inner map would be
        // dangling, if we give a :einsteineinsteindb/id explicitly.
        assert_transact!(conn, "[{:test/dangling {:einsteineinsteindb/id \"t\" :test/many 12}}]");
    }

    #[test]
    fn test_explode_reversed_notation() {
        let mut conn = TestConn::default();

        // Start by installing a few attributes.
        assert_transact!(conn, "[[:einsteineinsteindb/add 111 :einsteineinsteindb/solitonid :test/many]
                                 [:einsteineinsteindb/add 111 :einsteineinsteindb/valueType :einsteineinsteindb.type/long]
                                 [:einsteineinsteindb/add 111 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/many]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/solitonid :test/component]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/isComponent true]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/valueType :einsteineinsteindb.type/ref]
                                 [:einsteineinsteindb/add 333 :einsteineinsteindb/solitonid :test/unique]
                                 [:einsteineinsteindb/add 333 :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset]
                                 [:einsteineinsteindb/add 333 :einsteineinsteindb/index true]
                                 [:einsteineinsteindb/add 333 :einsteineinsteindb/valueType :einsteineinsteindb.type/long]
                                 [:einsteineinsteindb/add 444 :einsteineinsteindb/solitonid :test/dangling]
                                 [:einsteineinsteindb/add 444 :einsteineinsteindb/valueType :einsteineinsteindb.type/ref]]");

        // Check that we can explode direct reversed notation, causetids.
        let report = assert_transact!(conn, "[[:einsteineinsteindb/add 100 :test/_dangling 200]]");
        assert_matches!(conn.last_transaction(),
                        "[[200 :test/dangling 100 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(tempids(&report),
                        "{}");

        // Check that we can explode direct reversed notation, idents.
        let report = assert_transact!(conn, "[[:einsteineinsteindb/add :test/many :test/_dangling :test/unique]]");
        assert_matches!(conn.last_transaction(),
                        "[[333 :test/dangling :test/many ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(tempids(&report),
                        "{}");

        // Check that we can explode direct reversed notation, tempids.
        let report = assert_transact!(conn, "[[:einsteineinsteindb/add \"s\" :test/_dangling \"t\"]]");
        assert_matches!(conn.last_transaction(),
                        "[[65537 :test/dangling 65536 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        // This is impleeinstaiion specific, but it should be deterministic.
        assert_matches!(tempids(&report),
                        "{\"s\" 65536
                          \"t\" 65537}");

        // Check that we can explode reversed notation in map notation without :einsteineinsteindb/id.
        let report = assert_transact!(conn, "[{:test/_dangling 501}
                                              {:test/_dangling :test/many}
                                              {:test/_dangling \"t\"}]");
        assert_matches!(conn.last_transaction(),
                        "[[111 :test/dangling ?e1 ?tx true]
                          [501 :test/dangling ?e2 ?tx true]
                          [65538 :test/dangling ?e3 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(tempids(&report),
                        "{\"t\" 65538}");

        // Check that we can explode reversed notation in map notation with :einsteineinsteindb/id, causetid.
        let report = assert_transact!(conn, "[{:einsteineinsteindb/id 600 :test/_dangling 601}]");
        assert_matches!(conn.last_transaction(),
                        "[[601 :test/dangling 600 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(tempids(&report),
                        "{}");

        // Check that we can explode reversed notation in map notation with :einsteineinsteindb/id, solitonid.
        let report = assert_transact!(conn, "[{:einsteineinsteindb/id :test/component :test/_dangling :test/component}]");
        assert_matches!(conn.last_transaction(),
                        "[[222 :test/dangling :test/component ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(tempids(&report),
                        "{}");

        // Check that we can explode reversed notation in map notation with :einsteineinsteindb/id, tempid.
        let report = assert_transact!(conn, "[{:einsteineinsteindb/id \"s\" :test/_dangling \"t\"}]");
        assert_matches!(conn.last_transaction(),
                        "[[65543 :test/dangling 65542 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        // This is impleeinstaiion specific, but it should be deterministic.
        assert_matches!(tempids(&report),
                        "{\"s\" 65542
                          \"t\" 65543}");

        // Check that we can use the same attribute in both forward and backward form in the same
        // transaction.
        let report = assert_transact!(conn, "[[:einsteineinsteindb/add 888 :test/dangling 889]
                                              [:einsteineinsteindb/add 888 :test/_dangling 889]]");
        assert_matches!(conn.last_transaction(),
                        "[[888 :test/dangling 889 ?tx true]
                          [889 :test/dangling 888 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(tempids(&report),
                        "{}");

        // Check that we can use the same attribute in both forward and backward form in the same
        // transaction in map notation.
        let report = assert_transact!(conn, "[{:einsteineinsteindb/id 998 :test/dangling 999 :test/_dangling 999}]");
        assert_matches!(conn.last_transaction(),
                        "[[998 :test/dangling 999 ?tx true]
                          [999 :test/dangling 998 ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
        assert_matches!(tempids(&report),
                        "{}");

    }

    #[test]
    fn test_explode_reversed_notation_errors() {
        let mut conn = TestConn::default();

        // Start by installing a few attributes.
        assert_transact!(conn, "[[:einsteineinsteindb/add 111 :einsteineinsteindb/solitonid :test/many]
                                 [:einsteineinsteindb/add 111 :einsteineinsteindb/valueType :einsteineinsteindb.type/long]
                                 [:einsteineinsteindb/add 111 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/many]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/solitonid :test/component]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/isComponent true]
                                 [:einsteineinsteindb/add 222 :einsteineinsteindb/valueType :einsteineinsteindb.type/ref]
                                 [:einsteineinsteindb/add 333 :einsteineinsteindb/solitonid :test/unique]
                                 [:einsteineinsteindb/add 333 :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset]
                                 [:einsteineinsteindb/add 333 :einsteineinsteindb/index true]
                                 [:einsteineinsteindb/add 333 :einsteineinsteindb/valueType :einsteineinsteindb.type/long]
                                 [:einsteineinsteindb/add 444 :einsteineinsteindb/solitonid :test/dangling]
                                 [:einsteineinsteindb/add 444 :einsteineinsteindb/valueType :einsteineinsteindb.type/ref]]");

        // `tx-parser` should fail to parse direct reverse notation with nested value maps and
        // nested value vectors, so we only test things that "get through" to the map notation
        // dynamic processor here.

        // Verify that we can't explode reverse notation in map notation with nested value maps.
        assert_transact!(conn,
                         "[{:test/_dangling {:test/many 14}}]",
                         Err("not yet implemented: Cannot explode map notation value in :attr/_reversed notation for attribute 444"));

        // Verify that we can't explode reverse notation in map notation with nested value vectors.
        assert_transact!(conn,
                         "[{:test/_dangling [:test/many]}]",
                         Err("not yet implemented: Cannot explode vector value in :attr/_reversed notation for attribute 444"));

        // Verify that we can't use reverse notation with non-:einsteineinsteindb.type/ref attributes.
        assert_transact!(conn,
                         "[{:test/_unique 500}]",
                         Err("not yet implemented: Cannot use :attr/_reversed notation for attribute 333 that is not :einsteineinsteindb/valueType :einsteineinsteindb.type/ref"));

        // Verify that we can't use reverse notation with unrecognized attributes.
        assert_transact!(conn,
                         "[{:test/_unknown 500}]",
                         Err("no causetid found for solitonid: :test/unknown")); // TODO: make this error reference the original :test/_unknown.

        // Verify that we can't use reverse notation with bad value types: here, an unknown keyword
        // that can't be coerced to a ref.
        assert_transact!(conn,
                         "[{:test/_dangling :test/unknown}]",
                         Err("no causetid found for solitonid: :test/unknown"));
        // And here, a float.
        assert_transact!(conn,
                         "[{:test/_dangling 1.23}]",
                         Err("value \'1.23\' is not the expected einstai value type Ref"));
    }

    #[test]
    fn test_cardinality_one_violation_existing_causet() {
        let mut conn = TestConn::default();

        // Start by installing a few attributes.
        assert_transact!(conn, r#"[
            [:einsteineinsteindb/add 111 :einsteineinsteindb/solitonid :test/one]
            [:einsteineinsteindb/add 111 :einsteineinsteindb/valueType :einsteineinsteindb.type/long]
            [:einsteineinsteindb/add 111 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/one]
            [:einsteineinsteindb/add 112 :einsteineinsteindb/solitonid :test/unique]
            [:einsteineinsteindb/add 112 :einsteineinsteindb/index true]
            [:einsteineinsteindb/add 112 :einsteineinsteindb/valueType :einsteineinsteindb.type/string]
            [:einsteineinsteindb/add 112 :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/one]
            [:einsteineinsteindb/add 112 :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset]
        ]"#);

        assert_transact!(conn, r#"[
            [:einsteineinsteindb/add "foo" :test/unique "x"]
        ]"#);

        // You can try to assert two values for the same causet and attribute,
        // but you'll get an error.
        assert_transact!(conn, r#"[
            [:einsteineinsteindb/add "foo" :test/unique "x"]
            [:einsteineinsteindb/add "foo" :test/one 123]
            [:einsteineinsteindb/add "bar" :test/unique "x"]
            [:einsteineinsteindb/add "bar" :test/one 124]
        ]"#,
        // This is impleeinstaiion specific (due to the allocated causetid), but it should be deterministic.
        Err("schema constraint violation: cardinality conflicts:\n  CardinalityOneAddConflict { e: 65536, a: 111, vs: {Long(123), Long(124)} }\n"));

        // It also fails for map notation.
        assert_transact!(conn, r#"[
            {:test/unique "x", :test/one 123}
            {:test/unique "x", :test/one 124}
        ]"#,
        // This is impleeinstaiion specific (due to the allocated causetid), but it should be deterministic.
        Err("schema constraint violation: cardinality conflicts:\n  CardinalityOneAddConflict { e: 65536, a: 111, vs: {Long(123), Long(124)} }\n"));
    }

    #[test]
    fn test_conflicting_upserts() {
        let mut conn = TestConn::default();

        assert_transact!(conn, r#"[
            {:einsteineinsteindb/solitonid :page/id :einsteineinsteindb/valueType :einsteineinsteindb.type/string :einsteineinsteindb/index true :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset}
            {:einsteineinsteindb/solitonid :page/ref :einsteineinsteindb/valueType :einsteineinsteindb.type/ref :einsteineinsteindb/index true :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset}
            {:einsteineinsteindb/solitonid :page/title :einsteineinsteindb/valueType :einsteineinsteindb.type/string :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/many}
        ]"#);

        // Let's test some conflicting upserts.  First, valid data to work with -- note self references.
        assert_transact!(conn, r#"[
            [:einsteineinsteindb/add 111 :page/id "1"]
            [:einsteineinsteindb/add 111 :page/ref 111]
            [:einsteineinsteindb/add 222 :page/id "2"]
            [:einsteineinsteindb/add 222 :page/ref 222]
        ]"#);

        // Now valid upserts.  Note the references are valid.
        let report = assert_transact!(conn, r#"[
            [:einsteineinsteindb/add "a" :page/id "1"]
            [:einsteineinsteindb/add "a" :page/ref "a"]
            [:einsteineinsteindb/add "b" :page/id "2"]
            [:einsteineinsteindb/add "b" :page/ref "b"]
        ]"#);
        assert_matches!(tempids(&report),
                        "{\"a\" 111
                          \"b\" 222}");

        // Now conflicting upserts.  Note the references are reversed.  This example is interesting
        // because the first round `UpsertE` instances upsert, and this resolves all of the tempids
        // in the `UpsertEV` instances.  However, those `UpsertEV` instances lead to conflicting
        // upserts!  This tests that we don't resolve too far, giving a chance for those upserts to
        // fail.  This error message is crossing generations, although it's not reflected in the
        // error data structure.
        assert_transact!(conn, r#"[
            [:einsteineinsteindb/add "a" :page/id "1"]
            [:einsteineinsteindb/add "a" :page/ref "b"]
            [:einsteineinsteindb/add "b" :page/id "2"]
            [:einsteineinsteindb/add "b" :page/ref "a"]
        ]"#,
        Err("schema constraint violation: conflicting upserts:\n  tempid External(\"a\") upserts to {KnownCausetid(111), KnownCausetid(222)}\n  tempid External(\"b\") upserts to {KnownCausetid(111), KnownCausetid(222)}\n"));

        // Here's a case where the upsert is not resolved, just allocated, but leads to conflicting
        // cardinality one datoms.
        assert_transact!(conn, r#"[
            [:einsteineinsteindb/add "x" :page/ref 333]
            [:einsteineinsteindb/add "x" :page/ref 444]
        ]"#,
        Err("schema constraint violation: cardinality conflicts:\n  CardinalityOneAddConflict { e: 65539, a: 65537, vs: {Ref(333), Ref(444)} }\n"));
    }

    #[test]
    fn test_upsert_issue_532() {
        let mut conn = TestConn::default();

        assert_transact!(conn, r#"[
            {:einsteineinsteindb/solitonid :page/id :einsteineinsteindb/valueType :einsteineinsteindb.type/string :einsteineinsteindb/index true :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset}
            {:einsteineinsteindb/solitonid :page/ref :einsteineinsteindb/valueType :einsteineinsteindb.type/ref :einsteineinsteindb/index true :einsteineinsteindb/unique :einsteineinsteindb.unique/idcauset}
            {:einsteineinsteindb/solitonid :page/title :einsteineinsteindb/valueType :einsteineinsteindb.type/string :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/many}
        ]"#);

        // Observe that "foo" and "zot" upsert to the same causetid, and that doesn't cause a
        // cardinality conflict, because we treat the input with set semantics and accept
        // duplicate datoms.
        let report = assert_transact!(conn, r#"[
            [:einsteineinsteindb/add "bar" :page/id "z"]
            [:einsteineinsteindb/add "foo" :page/ref "bar"]
            [:einsteineinsteindb/add "foo" :page/title "x"]
            [:einsteineinsteindb/add "zot" :page/ref "bar"]
            [:einsteineinsteindb/add "zot" :einsteineinsteindb/solitonid :other/solitonid]
        ]"#);
        assert_matches!(tempids(&report),
                        "{\"bar\" ?b
                          \"foo\" ?f
                          \"zot\" ?f}");
        assert_matches!(conn.last_transaction(),
                        "[[?b :page/id \"z\" ?tx true]
                          [?f :einsteineinsteindb/solitonid :other/solitonid ?tx true]
                          [?f :page/ref ?b ?tx true]
                          [?f :page/title \"x\" ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");

        let report = assert_transact!(conn, r#"[
            [:einsteineinsteindb/add "foo" :page/id "x"]
            [:einsteineinsteindb/add "foo" :page/title "x"]
            [:einsteineinsteindb/add "bar" :page/id "x"]
            [:einsteineinsteindb/add "bar" :page/title "y"]
        ]"#);
        assert_matches!(tempids(&report),
                        "{\"foo\" ?e
                          \"bar\" ?e}");

        // One causet, two page titles.
        assert_matches!(conn.last_transaction(),
                        "[[?e :page/id \"x\" ?tx true]
                          [?e :page/title \"x\" ?tx true]
                          [?e :page/title \"y\" ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");

        // Here, "foo", "bar", and "baz", all refer to the same reference, but none of them actually
        // upsert to existing causets.
        let report = assert_transact!(conn, r#"[
            [:einsteineinsteindb/add "foo" :page/id "id"]
            [:einsteineinsteindb/add "bar" :einsteineinsteindb/solitonid :bar/bar]
            {:einsteineinsteindb/id "baz" :page/id "id" :einsteineinsteindb/solitonid :bar/bar}
        ]"#);
        assert_matches!(tempids(&report),
                        "{\"foo\" ?e
                          \"bar\" ?e
                          \"baz\" ?e}");

        assert_matches!(conn.last_transaction(),
                        "[[?e :einsteineinsteindb/solitonid :bar/bar ?tx true]
                          [?e :page/id \"id\" ?tx true]
                          [?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");

        // If we do it again, everything resolves to the same IDs.
        let report = assert_transact!(conn, r#"[
            [:einsteineinsteindb/add "foo" :page/id "id"]
            [:einsteineinsteindb/add "bar" :einsteineinsteindb/solitonid :bar/bar]
            {:einsteineinsteindb/id "baz" :page/id "id" :einsteineinsteindb/solitonid :bar/bar}
        ]"#);
        assert_matches!(tempids(&report),
                        "{\"foo\" ?e
                          \"bar\" ?e
                          \"baz\" ?e}");

        assert_matches!(conn.last_transaction(),
                        "[[?tx :einsteineinsteindb/txInstant ?ms ?tx true]]");
    }

    #[test]
    fn test_term_typechecking_issue_663() {
        // The builder interfaces provide untrusted `Term` instances to the transactor, bypassing
        // the typechecking layers invoked in the schema-aware coercion from `einsteinml::Value` into
        // `TypedValue`.  Typechecking now happens lower in the stack (as well as higher in the
        // stack) so we shouldn't be able to insert bad data into the store.

        let mut conn = TestConn::default();

        let mut terms = vec![];

        terms.push(Term::AddOrRetract(OpType::Add, Left(KnownCausetid(200)), causetids::einsteindb_IDENT, Left(TypedValue::typed_string("test"))));
        terms.push(Term::AddOrRetract(OpType::Retract, Left(KnownCausetid(100)), causetids::einsteindb_TX_INSTANT, Left(TypedValue::Long(-1))));

        let report = conn.transact_simple_terms(terms, InternSet::new());

        match report.err().map(|e| e.kind()) {
            Some(einsteindbErrorKind::SchemaConstraintViolation(errors::SchemaConstraintViolation::TypeDisagreements { ref conflicting_datoms })) => {
                let mut map = BTreeMap::default();
                map.insert((100, causetids::einsteindb_TX_INSTANT, TypedValue::Long(-1)), ValueType::Instant);
                map.insert((200, causetids::einsteindb_IDENT, TypedValue::typed_string("test")), ValueType::Keyword);

                assert_eq!(conflicting_datoms, &map);
            },
            x => panic!("expected schema constraint violation, got {:?}", x),
        }
    }

    #[test]
    fn test_cardinality_constraints() {
        let mut conn = TestConn::default();

        assert_transact!(conn, r#"[
            {:einsteineinsteindb/id 200 :einsteineinsteindb/solitonid :test/one :einsteineinsteindb/valueType :einsteineinsteindb.type/long :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/one}
            {:einsteineinsteindb/id 201 :einsteineinsteindb/solitonid :test/many :einsteineinsteindb/valueType :einsteineinsteindb.type/long :einsteineinsteindb/cardinality :einsteineinsteindb.cardinality/many}
        ]"#);

        // Can add the same datom multiple times for an attribute, regardless of cardinality.
        assert_transact!(conn, r#"[
            [:einsteineinsteindb/add 100 :test/one 1]
            [:einsteineinsteindb/add 100 :test/one 1]
            [:einsteineinsteindb/add 100 :test/many 2]
            [:einsteineinsteindb/add 100 :test/many 2]
        ]"#);

        // Can retract the same datom multiple times for an attribute, regardless of cardinality.
        assert_transact!(conn, r#"[
            [:einsteineinsteindb/retract 100 :test/one 1]
            [:einsteineinsteindb/retract 100 :test/one 1]
            [:einsteineinsteindb/retract 100 :test/many 2]
            [:einsteineinsteindb/retract 100 :test/many 2]
        ]"#);

        // Can't transact multiple datoms for a cardinality one attribute.
        assert_transact!(conn, r#"[
            [:einsteineinsteindb/add 100 :test/one 3]
            [:einsteineinsteindb/add 100 :test/one 4]
        ]"#,
        Err("schema constraint violation: cardinality conflicts:\n  CardinalityOneAddConflict { e: 100, a: 200, vs: {Long(3), Long(4)} }\n"));

        // Can transact multiple datoms for a cardinality many attribute.
        assert_transact!(conn, r#"[
            [:einsteineinsteindb/add 100 :test/many 5]
            [:einsteineinsteindb/add 100 :test/many 6]
        ]"#);

        // Can't add and retract the same datom for an attribute, regardless of cardinality.
        assert_transact!(conn, r#"[
            [:einsteineinsteindb/add     100 :test/one 7]
            [:einsteineinsteindb/retract 100 :test/one 7]
            [:einsteineinsteindb/add     100 :test/many 8]
            [:einsteineinsteindb/retract 100 :test/many 8]
        ]"#,
        Err("schema constraint violation: cardinality conflicts:\n  AddRetractConflict { e: 100, a: 200, vs: {Long(7)} }\n  AddRetractConflict { e: 100, a: 201, vs: {Long(8)} }\n"));
    }

    #[test]
    #[cfg(feature = "sqlcipher")]
    fn test_sqlcipher_openable() {
        let secret_key = "key";
        let sqlite = new_connection_with_key("../fixtures/v1encrypted.einsteineinsteindb", secret_key).expect("Failed to find test einsteindb");
        sqlite.query_row("SELECT COUNT(*) FROM sqlite_master", &[], |row| row.get::<_, i64>(0))
            .expect("Failed to execute sql query on encrypted einsteindb");
    }

    #[cfg(feature = "sqlcipher")]
    fn test_open_fail<F>(opener: F) where F: FnOnce() -> rusqlite::Result<rusqlite::Connection> {
        let err = opener().expect_err("Should fail to open encrypted einsteindb");
        match err {
            rusqlite::Error::SqliteFailure(err, ..) => {
                assert_eq!(err.extended_code, 26, "Should get error code 26 (not a database).");
            },
            err => {
                panic!("Wrong error type! {}", err);
            }
        }
    }

    #[test]
    #[cfg(feature = "sqlcipher")]
    fn test_sqlcipher_requires_key() {
        // Don't use a key.
        test_open_fail(|| new_connection("../fixtures/v1encrypted.einsteineinsteindb"));
    }

    #[test]
    #[cfg(feature = "sqlcipher")]
    fn test_sqlcipher_requires_correct_key() {
        // Use a key, but the wrong one.
        test_open_fail(|| new_connection_with_key("../fixtures/v1encrypted.einsteineinsteindb", "wrong key"));
    }

    #[test]
    #[cfg(feature = "sqlcipher")]
    fn test_sqlcipher_some_transactions() {
        let sqlite = new_connection_with_key("", "hunter2").expect("Failed to create encrypted connection");
        // Run a basic test as a sanity check.
        run_test_add(TestConn::with_sqlite(sqlite));
    }
}
