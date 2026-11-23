//! `browser_fill_login` (context.md K57): fills the user's saved login for
//! the page's site into its sign-in form. The agent names the login by its
//! user name when the site has more than one; it never gets the password:
//! the fill runs in the password watcher's isolated world with the value
//! as an argument, and every answer is masked ([`crate::browser::passwords::mask`]).

use serde_json::Value;

use super::{Ctx, Reply, act, effects, invalid, quote};
use crate::browser::{
    ToolOutput,
    passwords::{
        origin_of,
        watch::{FillTarget, fill},
    },
};

fn no_login(message: impl Into<String>) -> ToolOutput {
    ToolOutput::error("browser_no_login", message.into())
}

pub async fn fill_login(ctx: &Ctx, args: &Value) -> Reply {
    let url = ctx.page.info().url;
    let Some(origin) = origin_of(&url) else {
        return Err(no_login(format!(
            "page {} is not a web site ({url}); nothing to fill",
            ctx.id
        )));
    };
    let logins = ctx.page.saved_logins().map_err(no_login)?;
    if logins.is_empty() {
        return Err(no_login(format!(
            "the user has no saved login for {origin}; ask them to sign in or to save one"
        )));
    }
    let wanted = args
        .get("username")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|u| !u.is_empty());
    let names = || {
        logins
            .iter()
            .map(|l| quote(&l.username, 80))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let login = match wanted {
        Some(user) => logins.iter().find(|l| l.username == user).ok_or_else(|| {
            no_login(format!(
                "no saved login for {} at {origin}; the saved ones are {}",
                quote(user, 80),
                names()
            ))
        })?,
        None if logins.len() == 1 => &logins[0],
        None => {
            return Err(invalid(format!(
                "{origin} has {} saved logins: {}; say which with username",
                logins.len(),
                names()
            )));
        }
    };
    let target = match act::ref_arg(args) {
        Some(r) => {
            let entry = act::resolve(ctx, &r).await?;
            FillTarget::Node {
                backend: entry.backend,
                session: entry.session.clone(),
                frame: entry.frame_id.clone(),
            }
        }
        None => FillTarget::Page,
    };
    let before = effects::before(ctx).await?;
    let filled = fill(&ctx.cdp, &target, &login.username, &login.password)
        .await
        .map_err(|why| {
            no_login(format!(
                "could not fill the login into page {}: {why}",
                ctx.id
            ))
        })?;
    ctx.page.login_used(&login.username);
    let what = match (filled.username, filled.password) {
        (true, true) => "user name and password",
        (false, true) => "password",
        _ => "user name",
    };
    let said = format!(
        "Filled the saved login {} for {origin} ({what}); the password is hidden from you. Submit the form to sign in.",
        quote(&login.username, 80)
    );
    let effects = effects::settle(ctx, before).await?;
    Ok(effects.with(said, true))
}
