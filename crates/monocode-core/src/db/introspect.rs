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
                "create schema if not exists mc_probe; \
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
            conn.execute("drop schema mc_probe cascade", 1).await.unwrap();
        });
    }
}
