//! The object tree from `pg_catalog`: schemas, tables and their columns.

use super::conn::{Connection, DbError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableKind {
    Table,
    View,
    MaterializedView,
    Foreign,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableInfo {
    pub name: String,
    pub kind: TableKind,
    /// The planner's row estimate (`reltuples`); `None` when never analyzed.
    pub estimated_rows: Option<u64>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnInfo {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub primary_key: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexInfo {
    pub name: String,
    pub definition: String,
    pub unique: bool,
    pub primary: bool,
}

/// A constraint of the table: `kind` is Postgres' `contype`
/// (`p` primary key, `u` unique, `f` foreign key, `c` check, `x` exclusion).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstraintInfo {
    pub name: String,
    pub kind: char,
    pub definition: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerInfo {
    pub name: String,
    pub definition: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutineKind {
    Function,
    Procedure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineInfo {
    /// `pg_proc.oid`, which tells overloads apart.
    pub oid: String,
    pub name: String,
    /// `a integer, b text`.
    pub arguments: String,
    pub kind: RoutineKind,
    /// The return type; empty for a procedure.
    pub result: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceInfo {
    pub name: String,
    pub data_type: String,
    pub start: String,
    pub increment: String,
}

/// The objects of a schema that are not tables.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchemaObjects {
    pub routines: Vec<RoutineInfo>,
    pub sequences: Vec<SequenceInfo>,
}

/// Everything about a table beyond its columns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableDetails {
    pub indexes: Vec<IndexInfo>,
    pub constraints: Vec<ConstraintInfo>,
    /// Foreign keys of other tables that point here: (table, definition).
    pub referenced_by: Vec<(String, String)>,
    pub triggers: Vec<TriggerInfo>,
}

/// An identifier, quoted only when it has to be.
pub fn quote(name: &str) -> String {
    let plain = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit());
    if plain {
        name.to_owned()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

/// A string as a SQL literal.
pub fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// One column of one relation, for completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub schema: String,
    pub table: String,
    pub kind: TableKind,
    pub column: String,
    pub type_name: String,
}

impl Connection {
    /// Every user column of every relation (capped), so completion knows
    /// schemas, tables and columns without a round trip per keystroke.
    pub async fn catalog(&self) -> Result<Vec<CatalogEntry>, DbError> {
        let rows = self
            .rows(
                "select n.nspname, c.relname, c.relkind::text, a.attname, \
                        format_type(a.atttypid, a.atttypmod) \
                 from pg_attribute a \
                 join pg_class c on c.oid = a.attrelid \
                 join pg_namespace n on n.oid = c.relnamespace \
                 where a.attnum > 0 and not a.attisdropped \
                   and c.relkind in ('r', 'p', 'v', 'm', 'f') \
                   and n.nspname not in ('pg_catalog', 'information_schema') \
                   and n.nspname not like 'pg\\_toast%' and n.nspname not like 'pg\\_temp%' \
                 order by n.nspname, c.relname, a.attnum \
                 limit 60000",
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| CatalogEntry {
                schema: r[0].clone().unwrap_or_default(),
                table: r[1].clone().unwrap_or_default(),
                kind: match r[2].as_deref() {
                    Some("v") => TableKind::View,
                    Some("m") => TableKind::MaterializedView,
                    Some("f") => TableKind::Foreign,
                    _ => TableKind::Table,
                },
                column: r[3].clone().unwrap_or_default(),
                type_name: r[4].clone().unwrap_or_default(),
            })
            .collect())
    }

    pub async fn schemas(&self, system: bool) -> Result<Vec<String>, DbError> {
        let filter = if system {
            ""
        } else {
            "where nspname not in ('pg_catalog', 'information_schema') \
             and nspname not like 'pg\\_toast%' and nspname not like 'pg\\_temp%'"
        };
        let rows = self
            .rows(&format!(
                "select nspname from pg_namespace {filter} order by nspname"
            ))
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|mut r| r.swap_remove(0))
            .collect())
    }

    pub async fn tables(&self, schema: &str) -> Result<Vec<TableInfo>, DbError> {
        let rows = self
            .rows(&format!(
                "select c.relname, c.relkind::text, c.reltuples::bigint::text, d.description \
                 from pg_class c join pg_namespace n on n.oid = c.relnamespace \
                 left join pg_description d on d.objoid = c.oid and d.objsubid = 0 \
                 where n.nspname = {} and c.relkind in ('r', 'p', 'v', 'm', 'f') \
                 order by c.relname",
                literal(schema)
            ))
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| TableInfo {
                name: r[0].clone().unwrap_or_default(),
                kind: match r[1].as_deref() {
                    Some("v") => TableKind::View,
                    Some("m") => TableKind::MaterializedView,
                    Some("f") => TableKind::Foreign,
                    _ => TableKind::Table,
                },
                estimated_rows: r[2]
                    .as_deref()
                    .and_then(|n| n.parse::<i64>().ok())
                    .and_then(|n| u64::try_from(n).ok()),
                comment: r[3].clone().filter(|c| !c.is_empty()),
            })
            .collect())
    }

    /// Functions, procedures and sequences of `schema` (extension-owned
    /// routines are left out).
    pub async fn schema_objects(&self, schema: &str) -> Result<SchemaObjects, DbError> {
        let routines = self
            .rows(&format!(
                "select p.oid::text, p.proname, pg_get_function_identity_arguments(p.oid), p.prokind::text, \
                        coalesce(pg_get_function_result(p.oid), '') \
                 from pg_proc p join pg_namespace n on n.oid = p.pronamespace \
                 where n.nspname = {} and p.prokind in ('f', 'p') \
                   and not exists (select 1 from pg_depend d where d.objid = p.oid and d.deptype = 'e') \
                 order by p.proname, 3",
                literal(schema)
            ))
            .await?;
        let sequences = self
            .rows(&format!(
                "select sequencename, data_type::text, start_value::text, increment_by::text \
                 from pg_sequences where schemaname = {} order by sequencename",
                literal(schema)
            ))
            .await?;
        let text = |v: &Option<String>| v.clone().unwrap_or_default();
        Ok(SchemaObjects {
            routines: routines
                .iter()
                .map(|r| RoutineInfo {
                    oid: text(&r[0]),
                    name: text(&r[1]),
                    arguments: text(&r[2]),
                    kind: if r[3].as_deref() == Some("p") {
                        RoutineKind::Procedure
                    } else {
                        RoutineKind::Function
                    },
                    result: text(&r[4]),
                })
                .collect(),
            sequences: sequences
                .iter()
                .map(|r| SequenceInfo {
                    name: text(&r[0]),
                    data_type: text(&r[1]),
                    start: text(&r[2]),
                    increment: text(&r[3]),
                })
                .collect(),
        })
    }

    /// The `CREATE FUNCTION` / `CREATE PROCEDURE` script of a routine.
    pub async fn routine_ddl(&self, oid: &str) -> Result<String, DbError> {
        let oid: u32 = oid.parse().map_err(|_| DbError {
            message: "Not a routine id".into(),
            ..DbError::default()
        })?;
        let rows = self
            .rows(&format!("select pg_get_functiondef({oid}::oid)"))
            .await?;
        let mut text = rows.first().and_then(|r| r[0].clone()).unwrap_or_default();
        if !text.trim_end().ends_with(';') {
            text = format!("{};", text.trim_end());
        }
        Ok(format!("{text}\n"))
    }

    /// A `CREATE SEQUENCE` script for a sequence.
    pub async fn sequence_ddl(&self, schema: &str, name: &str) -> Result<String, DbError> {
        let rows = self
            .rows(&format!(
                "select data_type::text, start_value::text, min_value::text, max_value::text, \
                        increment_by::text, cycle::text, cache_size::text \
                 from pg_sequences where schemaname = {} and sequencename = {}",
                literal(schema),
                literal(name)
            ))
            .await?;
        let Some(r) = rows.first() else {
            return Err(DbError {
                message: format!("Sequence {schema}.{name} not found"),
                ..DbError::default()
            });
        };
        let text = |i: usize| r[i].clone().unwrap_or_default();
        Ok(format!(
            "create sequence {}.{}\n    as {}\n    increment by {}\n    minvalue {}\n    maxvalue {}\n    start with {}\n    cache {}\n    {};\n",
            quote(schema),
            quote(name),
            text(0),
            text(4),
            text(2),
            text(3),
            text(1),
            text(6),
            if text(5) == "true" { "cycle" } else { "no cycle" }
        ))
    }

    pub async fn table_details(&self, schema: &str, table: &str) -> Result<TableDetails, DbError> {
        let relation = format!(
            "(select c.oid from pg_class c join pg_namespace n on n.oid = c.relnamespace \
              where n.nspname = {} and c.relname = {})",
            literal(schema),
            literal(table)
        );
        let indexes = self
            .rows(&format!(
                "select i.relname, pg_get_indexdef(i.oid), x.indisunique::text, x.indisprimary::text \
                 from pg_index x join pg_class i on i.oid = x.indexrelid \
                 where x.indrelid = {relation} order by x.indisprimary desc, i.relname"
            ))
            .await?;
        let constraints = self
            .rows(&format!(
                "select conname, contype::text, pg_get_constraintdef(oid) from pg_constraint \
                 where conrelid = {relation} order by contype, conname"
            ))
            .await?;
        let referenced_by = self
            .rows(&format!(
                "select c.conrelid::regclass::text, c.conname || ' ' || pg_get_constraintdef(c.oid) \
                 from pg_constraint c where c.contype = 'f' and c.confrelid = {relation} \
                 order by 1, 2"
            ))
            .await?;
        let triggers = self
            .rows(&format!(
                "select tgname, pg_get_triggerdef(oid) from pg_trigger \
                 where tgrelid = {relation} and not tgisinternal order by tgname"
            ))
            .await?;
        let text = |v: &Option<String>| v.clone().unwrap_or_default();
        Ok(TableDetails {
            indexes: indexes
                .iter()
                .map(|r| IndexInfo {
                    name: text(&r[0]),
                    definition: text(&r[1]),
                    unique: r[2].as_deref() == Some("true"),
                    primary: r[3].as_deref() == Some("true"),
                })
                .collect(),
            constraints: constraints
                .iter()
                .map(|r| ConstraintInfo {
                    name: text(&r[0]),
                    kind: text(&r[1]).chars().next().unwrap_or('?'),
                    definition: text(&r[2]),
                })
                .collect(),
            referenced_by: referenced_by
                .iter()
                .map(|r| (text(&r[0]), text(&r[1])))
                .collect(),
            triggers: triggers
                .iter()
                .map(|r| TriggerInfo {
                    name: text(&r[0]),
                    definition: text(&r[1]),
                })
                .collect(),
        })
    }

    /// A `CREATE` script for a table, view or materialized view, rebuilt
    /// from the catalog.
    pub async fn ddl(&self, schema: &str, table: &str, kind: TableKind) -> Result<String, DbError> {
        let qualified = format!("{}.{}", quote(schema), quote(table));
        let details = self.table_details(schema, table).await?;
        let mut out = String::new();
        match kind {
            TableKind::View | TableKind::MaterializedView => {
                let def = self
                    .rows(&format!(
                        "select pg_get_viewdef({}::regclass, true)",
                        literal(&qualified)
                    ))
                    .await?;
                let body = def.first().and_then(|r| r[0].clone()).unwrap_or_default();
                out.push_str(&format!(
                    "create {}view {qualified} as\n{}\n",
                    if kind == TableKind::MaterializedView {
                        "materialized "
                    } else {
                        ""
                    },
                    body.trim_end().trim_end_matches(';')
                ));
                out.push_str(";\n");
            }
            _ => {
                let columns = self.columns(schema, table).await?;
                out.push_str(&format!("create table {qualified} (\n"));
                let mut lines: Vec<String> = columns
                    .iter()
                    .map(|c| {
                        // A sequence-backed integer column reads best as serial.
                        let sequence = c
                            .default
                            .as_deref()
                            .is_some_and(|d| d.starts_with("nextval("));
                        let serial = match (sequence, c.type_name.as_str()) {
                            (true, "integer") => Some("serial"),
                            (true, "bigint") => Some("bigserial"),
                            (true, "smallint") => Some("smallserial"),
                            _ => None,
                        };
                        let mut line =
                            format!("    {} {}", quote(&c.name), serial.unwrap_or(&c.type_name));
                        if serial.is_none() {
                            if let Some(default) = &c.default {
                                line.push_str(&format!(" default {default}"));
                            }
                        }
                        if !c.nullable {
                            line.push_str(" not null");
                        }
                        line
                    })
                    .collect();
                lines.extend(
                    details
                        .constraints
                        .iter()
                        .map(|c| format!("    constraint {} {}", quote(&c.name), c.definition)),
                );
                out.push_str(&lines.join(",\n"));
                out.push_str("\n);\n");
            }
        }
        // Indexes that back a constraint are already part of it.
        let backing: Vec<&str> = details
            .constraints
            .iter()
            .filter(|c| matches!(c.kind, 'p' | 'u' | 'x'))
            .map(|c| c.name.as_str())
            .collect();
        for index in details
            .indexes
            .iter()
            .filter(|i| !backing.contains(&i.name.as_str()))
        {
            out.push_str(&format!("\n{};\n", index.definition));
        }
        for trigger in &details.triggers {
            out.push_str(&format!("\n{};\n", trigger.definition));
        }
        Ok(out)
    }

    pub async fn columns(&self, schema: &str, table: &str) -> Result<Vec<ColumnInfo>, DbError> {
        let rows = self
            .rows(&format!(
                "select a.attname, format_type(a.atttypid, a.atttypmod), (not a.attnotnull)::text, \
                        pg_get_expr(ad.adbin, ad.adrelid), \
                        coalesce(a.attnum = any(i.indkey), false)::text \
                 from pg_attribute a \
                 join pg_class c on c.oid = a.attrelid \
                 join pg_namespace n on n.oid = c.relnamespace \
                 left join pg_attrdef ad on ad.adrelid = a.attrelid and ad.adnum = a.attnum \
                 left join pg_index i on i.indrelid = c.oid and i.indisprimary \
                 where n.nspname = {} and c.relname = {} and a.attnum > 0 and not a.attisdropped \
                 order by a.attnum",
                literal(schema),
                literal(table)
            ))
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| ColumnInfo {
                name: r[0].clone().unwrap_or_default(),
                type_name: r[1].clone().unwrap_or_default(),
                nullable: r[2].as_deref() == Some("true"),
                default: r[3].clone(),
                primary_key: r[4].as_deref() == Some("true"),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::conn::tests::test_source;
    use crate::db::runtime::runtime;

    #[test]
    fn identifiers_are_quoted_only_when_needed() {
        assert_eq!(quote("orders"), "orders");
        assert_eq!(quote("Order Items"), "\"Order Items\"");
        assert_eq!(quote("a\"b"), "\"a\"\"b\"");
        assert_eq!(quote("1abc"), "\"1abc\"");
    }

    #[test]
    fn literals_double_their_quotes() {
        assert_eq!(literal("it's"), "'it''s'");
    }

    #[test]
    fn the_catalog_lists_schemas_tables_and_columns() {
        let Some(source) = test_source() else {
            return;
        };
        runtime().block_on(async {
            let conn = Connection::connect(&source, None).await.unwrap();
            conn.execute(
                "drop schema if exists mc_probe cascade; create schema mc_probe; \
                 drop table if exists mc_probe.t cascade; \
                 create table mc_probe.t (id int primary key, body jsonb not null default '{}', note text); \
                 create or replace view mc_probe.v as select id from mc_probe.t; \
                 comment on table mc_probe.t is 'probe'",
                1,
            )
            .await
            .unwrap();
            assert!(conn.schemas(false).await.unwrap().contains(&"mc_probe".to_owned()));
            assert!(!conn.schemas(false).await.unwrap().contains(&"pg_catalog".to_owned()));
            assert!(conn.schemas(true).await.unwrap().contains(&"pg_catalog".to_owned()));
            let tables = conn.tables("mc_probe").await.unwrap();
            assert_eq!(
                tables.iter().map(|t| (t.name.as_str(), t.kind)).collect::<Vec<_>>(),
                [("t", TableKind::Table), ("v", TableKind::View)]
            );
            assert_eq!(tables[0].comment.as_deref(), Some("probe"));
            let columns = conn.columns("mc_probe", "t").await.unwrap();
            assert_eq!(columns.len(), 3);
            assert!(columns[0].primary_key && !columns[0].nullable);
            assert_eq!(columns[1].type_name, "jsonb");
            assert!(columns[1].default.as_deref().unwrap().contains("'{}'"));
            assert!(columns[2].nullable && !columns[2].primary_key);
            let catalog = conn.catalog().await.unwrap();
            let mine: Vec<_> = catalog.iter().filter(|e| e.schema == "mc_probe" && e.table == "t").collect();
            assert_eq!(mine.iter().map(|e| e.column.as_str()).collect::<Vec<_>>(), ["id", "body", "note"]);
            assert!(catalog.iter().any(|e| e.schema == "mc_probe" && e.table == "v" && e.kind == TableKind::View));
            conn.execute(
                "create table mc_probe.child (id serial primary key, parent_id int not null references mc_probe.t(id), tag text unique, check (id > 0)); \
                 create index child_tag_idx on mc_probe.child (parent_id, tag); \
                 create function mc_probe.noop() returns trigger language plpgsql as $$ begin return new; end $$; \
                 create trigger child_noop before insert on mc_probe.child for each row execute function mc_probe.noop()",
                1,
            )
            .await
            .unwrap();
            let details = conn.table_details("mc_probe", "child").await.unwrap();
            assert!(details.indexes[0].primary, "the primary key index comes first");
            assert!(details.indexes.iter().any(|i| i.name == "child_tag_idx" && i.definition.contains("(parent_id, tag)")));
            let kinds: Vec<char> = details.constraints.iter().map(|c| c.kind).collect();
            assert!(kinds.contains(&'p') && kinds.contains(&'f') && kinds.contains(&'u') && kinds.contains(&'c'));
            assert!(details.constraints.iter().any(|c| c.kind == 'f' && c.definition.contains("REFERENCES mc_probe.t(id)")));
            assert_eq!(details.triggers.len(), 1);
            let parent = conn.table_details("mc_probe", "t").await.unwrap();
            assert!(parent.referenced_by.iter().any(|(table, def)| table.contains("child") && def.contains("FOREIGN KEY")));
            conn.execute(
                "create sequence mc_probe.ticket_seq as bigint increment by 5 start 100 cycle; \
                 create function mc_probe.add_one(a integer) returns integer language sql as $$ select a + 1 $$; \
                 create function mc_probe.add_one(a text) returns text language sql as $$ select a || '1' $$; \
                 create procedure mc_probe.reset_all() language sql as $$ select 1 $$",
                1,
            )
            .await
            .unwrap();
            let objects = conn.schema_objects("mc_probe").await.unwrap();
            let names: Vec<(&str, RoutineKind, &str)> = objects
                .routines
                .iter()
                .map(|r| (r.name.as_str(), r.kind, r.arguments.as_str()))
                .collect();
            assert!(names.contains(&("add_one", RoutineKind::Function, "a integer")), "{names:?}");
            assert!(names.contains(&("add_one", RoutineKind::Function, "a text")), "overloads are listed apart");
            assert!(names.contains(&("reset_all", RoutineKind::Procedure, "")));
            assert!(objects.routines.iter().all(|r| r.name != "noop" || r.kind == RoutineKind::Function));
            let seq = objects.sequences.iter().find(|s| s.name == "ticket_seq").unwrap();
            assert_eq!((seq.data_type.as_str(), seq.start.as_str(), seq.increment.as_str()), ("bigint", "100", "5"));
            let int_version = objects.routines.iter().find(|r| r.arguments == "a integer").unwrap();
            let fn_ddl = conn.routine_ddl(&int_version.oid).await.unwrap();
            assert!(fn_ddl.contains("FUNCTION mc_probe.add_one(a integer)") && fn_ddl.trim_end().ends_with(';'), "{fn_ddl}");
            let proc_ddl = conn.routine_ddl(&objects.routines.iter().find(|r| r.name == "reset_all").unwrap().oid).await.unwrap();
            assert!(proc_ddl.contains("PROCEDURE mc_probe.reset_all"), "{proc_ddl}");
            let seq_ddl = conn.sequence_ddl("mc_probe", "ticket_seq").await.unwrap();
            assert!(seq_ddl.contains("increment by 5") && seq_ddl.contains("start with 100") && seq_ddl.contains("cycle"), "{seq_ddl}");
            // The scripts run: drop the objects and recreate them from the scripts.
            conn.execute("drop sequence mc_probe.ticket_seq; drop procedure mc_probe.reset_all", 1).await.unwrap();
            conn.execute(&seq_ddl, 1).await.unwrap();
            conn.execute(&proc_ddl, 1).await.unwrap();
            let ddl = conn.ddl("mc_probe", "child", TableKind::Table).await.unwrap();
            assert!(ddl.starts_with("create table mc_probe.child ("), "{ddl}");
            assert!(ddl.contains("constraint") && ddl.contains("FOREIGN KEY") && ddl.contains("id serial"), "{ddl}");
            assert!(ddl.to_lowercase().contains("create index child_tag_idx"), "{ddl}");
            assert!(ddl.to_lowercase().contains("create trigger child_noop"), "{ddl}");
            // The script runs: drop and recreate the table from it.
            conn.execute("drop table mc_probe.child", 1).await.unwrap();
            conn.execute(&ddl, 1).await.unwrap();
            assert_eq!(conn.table_details("mc_probe", "child").await.unwrap().indexes.len(), details.indexes.len());
            let view = conn.ddl("mc_probe", "v", TableKind::View).await.unwrap();
            assert!(view.starts_with("create view mc_probe.v as"), "{view}");
            conn.execute("drop schema mc_probe cascade", 1).await.unwrap();
        });
    }
}
