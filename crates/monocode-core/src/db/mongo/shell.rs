//! Evaluating console statements. Each MongoDB connection has one shell: a
//! QuickJS context on a thread of its own (contexts cannot move between
//! threads), so variables a statement declares stay for the next ones, as in
//! mongosh. Evaluation never touches the network; it turns a statement into
//! a [`Plan`] that the connection runs with the driver.

use crate::db::conn::DbError;
use mongodb::bson::{Bson, Document};
use rquickjs::{Context, Function, Runtime};
use serde::Deserialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// How long one statement may evaluate (not run: that is the server's time).
const EVAL_BUDGET: Duration = Duration::from_secs(5);

/// A database call a statement asked for.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Plan {
    pub db: String,
    /// `None` for database methods (`db.runCommand`, `db.stats`…).
    pub collection: Option<String>,
    pub method: String,
    #[serde(default)]
    pub args: Vec<serde_json::Value>,
    /// Cursor modifiers in order: `sort`, `limit`, `explain`…
    #[serde(default)]
    pub chain: Vec<(String, Vec<serde_json::Value>)>,
}

/// A text command the shell takes outside JavaScript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Directive {
    Use(String),
    Show(String),
}

/// What evaluating a statement gave.
#[derive(Debug, Clone, PartialEq)]
pub enum Evaluated {
    Plan(Plan),
    Directive(Directive),
    /// A plain value (`1 + 1`, `ObjectId()`); `None` when undefined.
    Value(Option<serde_json::Value>, Vec<String>),
}

/// `use name` / `show what`, which are not JavaScript.
pub fn directive(statement: &str) -> Option<Directive> {
    let text = statement.trim().trim_end_matches(';').trim();
    let (word, rest) = text.split_once(char::is_whitespace)?;
    let rest = rest.trim();
    if rest.is_empty() || rest.contains(char::is_whitespace) {
        return None;
    }
    match word {
        "use" => Some(Directive::Use(rest.to_owned())),
        "show" => Some(Directive::Show(rest.to_lowercase())),
        _ => None,
    }
}

/// `const x = …` / `let x = …` at the start of a statement become `var`, so
/// running the line again redefines the name instead of failing.
fn redeclarable(statement: &str) -> String {
    let trimmed = statement.trim_start();
    for keyword in ["const ", "let "] {
        if let Some(rest) = trimmed.strip_prefix(keyword) {
            return format!("var {rest}");
        }
    }
    statement.to_owned()
}

impl Plan {
    /// Argument `i` as BSON; missing is `None`.
    pub fn arg(&self, i: usize) -> Result<Option<Bson>, DbError> {
        self.args.get(i).map(to_bson).transpose()
    }

    /// Argument `i` as a document; missing or null is an empty one.
    pub fn doc_arg(&self, i: usize, what: &str) -> Result<Document, DbError> {
        match self.arg(i)? {
            None | Some(Bson::Null) => Ok(Document::new()),
            Some(Bson::Document(doc)) => Ok(doc),
            Some(other) => Err(DbError::plain(format!(
                "{}(): {what} must be a document, not {}",
                self.method,
                super::value::type_name(&other)
            ))),
        }
    }

    /// The last value a modifier was given (`.limit(5)` → `5`).
    pub fn modifier(&self, name: &str) -> Option<&[serde_json::Value]> {
        self.chain
            .iter()
            .rev()
            .find(|(m, _)| m == name)
            .map(|(_, args)| args.as_slice())
    }

    pub fn has(&self, name: &str) -> bool {
        self.chain.iter().any(|(m, _)| m == name)
    }

    /// `db.users.find()` → `shop.users.find()`.
    pub fn describe(&self) -> String {
        match &self.collection {
            Some(c) => format!("{}.{c}.{}()", self.db, self.method),
            None => format!("{}.{}()", self.db, self.method),
        }
    }
}

/// Extended JSON from the shell into BSON.
pub fn to_bson(value: &serde_json::Value) -> Result<Bson, DbError> {
    Bson::try_from(value.clone())
        .map_err(|e| DbError::plain(format!("Not a valid BSON value: {e}")))
}

struct Job {
    source: String,
    db: String,
    reply: tokio::sync::oneshot::Sender<Result<Evaluated, DbError>>,
}

/// One QuickJS context on its own thread. Dropping the shell ends the thread.
pub struct Shell {
    jobs: mpsc::Sender<Job>,
}

impl Shell {
    pub fn new() -> Result<Self, DbError> {
        let (jobs, inbox) = mpsc::channel::<Job>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        std::thread::Builder::new()
            .name("monocode-mongo-shell".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let deadline = Arc::new(AtomicU64::new(u64::MAX));
                let started = Instant::now();
                let setup = || -> Result<(Runtime, Context), String> {
                    let runtime = Runtime::new().map_err(|e| e.to_string())?;
                    runtime.set_memory_limit(128 * 1024 * 1024);
                    runtime.set_max_stack_size(4 * 1024 * 1024);
                    let limit = deadline.clone();
                    runtime.set_interrupt_handler(Some(Box::new(move || {
                        started.elapsed().as_millis() as u64 > limit.load(Ordering::Relaxed)
                    })));
                    let context = Context::full(&runtime).map_err(|e| e.to_string())?;
                    context
                        .with(|ctx| ctx.eval::<(), _>(include_str!("prelude.js")))
                        .map_err(|e| format!("The MongoDB shell did not start: {e}"))?;
                    Ok((runtime, context))
                };
                let (_runtime, context) = match setup() {
                    Ok(pair) => {
                        let _ = ready_tx.send(Ok(()));
                        pair
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                while let Ok(job) = inbox.recv() {
                    let budget = started.elapsed() + EVAL_BUDGET;
                    deadline.store(budget.as_millis() as u64, Ordering::Relaxed);
                    let outcome = evaluate(&context, &job.source, &job.db);
                    deadline.store(u64::MAX, Ordering::Relaxed);
                    let _ = job.reply.send(outcome);
                }
            })
            .map_err(|e| DbError::plain(format!("The MongoDB shell did not start: {e}")))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { jobs }),
            Ok(Err(message)) => Err(DbError::plain(message)),
            Err(_) => Err(DbError::plain("The MongoDB shell did not start")),
        }
    }

    /// Evaluates `statement` with `db` bound to database `db`.
    pub async fn eval(&self, statement: &str, db: &str) -> Result<Evaluated, DbError> {
        if let Some(directive) = directive(statement) {
            return Ok(Evaluated::Directive(directive));
        }
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.jobs
            .send(Job {
                source: redeclarable(statement),
                db: db.to_owned(),
                reply,
            })
            .map_err(|_| DbError::plain("The MongoDB shell stopped"))?;
        answer
            .await
            .map_err(|_| DbError::plain("The MongoDB shell stopped"))?
    }
}

#[derive(Deserialize)]
struct Output {
    #[serde(default)]
    printed: Vec<String>,
    value: Option<serde_json::Value>,
}

const PLAN_KEY: &str = "__monocode_plan";

fn evaluate(context: &Context, source: &str, db: &str) -> Result<Evaluated, DbError> {
    let json = context.with(|ctx| {
        let run = || -> rquickjs::Result<String> {
            let function: Function = ctx.globals().get("__monocodeEval")?;
            function.call((source, db))
        };
        run().map_err(|error| match error {
            rquickjs::Error::Exception => {
                let caught = ctx.catch();
                let message = caught
                    .as_exception()
                    .map(|e| {
                        let name = e
                            .get::<_, String>("name")
                            .ok()
                            .filter(|n| n != "Error")
                            .map(|n| format!("{n}: "))
                            .unwrap_or_default();
                        format!("{name}{}", e.message().unwrap_or_default())
                    })
                    .or_else(|| caught.as_string().and_then(|s| s.to_string().ok()))
                    .unwrap_or_else(|| "The statement threw an exception".into());
                if message.contains("interrupted") {
                    "The statement took too long to evaluate".to_owned()
                } else {
                    message
                }
            }
            other => other.to_string(),
        })
    });
    let json = json.map_err(DbError::plain)?;
    let output: Output = serde_json::from_str(&json)
        .map_err(|e| DbError::plain(format!("The shell's answer could not be read: {e}")))?;
    if let Some(serde_json::Value::Object(map)) = &output.value {
        if let Some(plan) = map.get(PLAN_KEY) {
            let plan: Plan = serde_json::from_value(plan.clone())
                .map_err(|e| DbError::plain(format!("The shell's plan could not be read: {e}")))?;
            return Ok(Evaluated::Plan(plan));
        }
    }
    Ok(Evaluated::Value(output.value, output.printed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::doc;

    fn eval(shell: &Shell, text: &str) -> Result<Evaluated, DbError> {
        crate::db::runtime::runtime().block_on(shell.eval(text, "shop"))
    }

    fn plan(shell: &Shell, text: &str) -> Plan {
        match eval(shell, text).unwrap() {
            Evaluated::Plan(plan) => plan,
            other => panic!("not a plan: {other:?}"),
        }
    }

    #[test]
    fn statements_become_plans_with_bson_types() {
        let shell = Shell::new().unwrap();
        let p = plan(
            &shell,
            "db.users.find({ _id: ObjectId('65a1b2c3d4e5f60718293a4b'), at: { $gte: ISODate('2024-01-02T03:04:05Z') }, n: NumberLong(5), name: /^a/i })\n  .sort({ name: -1 }).limit(10)",
        );
        assert_eq!(
            (p.db.as_str(), p.collection.as_deref(), p.method.as_str()),
            ("shop", Some("users"), "find")
        );
        let filter = p.doc_arg(0, "the filter").unwrap();
        assert_eq!(
            filter.get_object_id("_id").unwrap().to_hex(),
            "65a1b2c3d4e5f60718293a4b"
        );
        assert_eq!(
            filter
                .get_document("at")
                .unwrap()
                .get_datetime("$gte")
                .unwrap()
                .timestamp_millis(),
            1_704_164_645_000
        );
        assert_eq!(filter.get_i64("n").unwrap(), 5);
        assert!(
            matches!(filter.get("name"), Some(Bson::RegularExpression(re)) if re.pattern == "^a" && re.options == "i")
        );
        assert_eq!(p.modifier("limit").unwrap(), [serde_json::json!(10)]);
        assert_eq!(
            to_bson(&p.modifier("sort").unwrap()[0]).unwrap(),
            Bson::Document(doc! { "name": -1 })
        );
    }

    #[test]
    fn variables_last_across_statements_and_names_resolve() {
        let shell = Shell::new().unwrap();
        assert!(matches!(
            eval(&shell, "const id = 42").unwrap(),
            Evaluated::Value(None, _)
        ));
        // Running the declaration again is fine.
        assert!(eval(&shell, "const id = 43").is_ok());
        let p = plan(
            &shell,
            "db.getSiblingDB('other').getCollection('a.b').findOne({ id })",
        );
        assert_eq!(
            (p.db.as_str(), p.collection.as_deref()),
            ("other", Some("a.b"))
        );
        assert_eq!(p.doc_arg(0, "f").unwrap(), doc! { "id": 43 });
        let p = plan(&shell, "db.system.profile.find()");
        assert_eq!(p.collection.as_deref(), Some("system.profile"));
        let p = plan(&shell, "db['my coll'].countDocuments()");
        assert_eq!(p.collection.as_deref(), Some("my coll"));
        let p = plan(&shell, "db.runCommand({ ping: 1 })");
        assert_eq!((p.collection, p.method.as_str()), (None, "runCommand"));
        let p = plan(&shell, "db.users.explain('executionStats').find({})");
        assert_eq!(
            p.modifier("explain").unwrap(),
            [serde_json::json!("executionStats")]
        );
    }

    #[test]
    fn values_prints_directives_and_errors() {
        let shell = Shell::new().unwrap();
        assert_eq!(
            eval(&shell, "1 + 1").unwrap(),
            Evaluated::Value(Some(serde_json::json!(2)), vec![])
        );
        assert_eq!(
            eval(&shell, "print('a', 1)").unwrap(),
            Evaluated::Value(None, vec!["a 1".into()])
        );
        assert_eq!(
            eval(&shell, "use admin").unwrap(),
            Evaluated::Directive(Directive::Use("admin".into()))
        );
        assert_eq!(
            eval(&shell, "show dbs").unwrap(),
            Evaluated::Directive(Directive::Show("dbs".into()))
        );
        let e = eval(&shell, "db.users.find({ a: ").unwrap_err();
        assert!(e.message.contains("SyntaxError"), "{e:?}");
        let e = eval(&shell, "ObjectId('nope')").unwrap_err();
        assert!(e.message.contains("24 hexadecimal"), "{e:?}");
        let e = eval(&shell, "db.users.find().forEach(printjson)").unwrap_err();
        assert!(e.message.contains("forEach"), "{e:?}");
        let e = eval(&shell, "while (true) {}").unwrap_err();
        assert!(e.message.contains("too long"), "{e:?}");
        // The shell still works after an interrupted statement.
        assert!(matches!(
            eval(&shell, "db.a.find()").unwrap(),
            Evaluated::Plan(_)
        ));
    }

    #[test]
    fn helpers_keep_their_types() {
        let shell = Shell::new().unwrap();
        let p = plan(
            &shell,
            "db.t.insertOne({ i: NumberInt(3), d: NumberDecimal('1.50'), f: Double(2), u: UUID('0123456789abcdef0123456789abcdef'), ts: Timestamp(5, 1), small: 7, big: 3.25 })",
        );
        let doc = p.doc_arg(0, "doc").unwrap();
        assert_eq!(doc.get("i"), Some(&Bson::Int32(3)));
        assert!(matches!(doc.get("d"), Some(Bson::Decimal128(_))));
        assert_eq!(doc.get("f"), Some(&Bson::Double(2.0)));
        assert!(matches!(doc.get("u"), Some(Bson::Binary(b)) if b.bytes.len() == 16));
        assert!(
            matches!(doc.get("ts"), Some(Bson::Timestamp(t)) if t.time == 5 && t.increment == 1)
        );
        assert_eq!(doc.get("small"), Some(&Bson::Int32(7)));
        assert_eq!(doc.get("big"), Some(&Bson::Double(3.25)));
        let p = plan(
            &shell,
            "db.t.insertOne({ _id: ObjectId(), at: new Date(0) })",
        );
        let doc = p.doc_arg(0, "doc").unwrap();
        assert!(doc.get_object_id("_id").is_ok());
        assert_eq!(doc.get_datetime("at").unwrap().timestamp_millis(), 0);
    }
}
